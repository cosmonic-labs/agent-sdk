//! The scripted model: each `chat` call gets the test's next reply.
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::sync::{mpsc, oneshot};
use wasmtime::StoreContextMut;
use wasmtime::component::{
    Accessor, Destination, FutureConsumer, FutureReader, HasSelf, Resource, Source, StreamConsumer,
    StreamProducer, StreamReader, StreamResult,
};

use crate::host::{ModelHandle, Scripted, State, chat, models, run_types, tool_types, types};
use types::{
    AssistantContent, AssistantMessage, AssistantPart, Chunk, ChunkDelta, Completion, FinishReason,
    GenerationError, Message, RequestError, ToolArguments, ToolCall, Usage,
};

pub const MODEL: &str = "scripted-model";

fn description() -> types::ModelDescription {
    types::ModelDescription {
        model: Some(MODEL.into()),
        supported_operations: types::SupportedOperations::CHAT,
        context_length: Some(32768),
        max_output_tokens: Some(4096),
        tools: Some(true),
        reasoning: Some(true),
        accepts: Some(vec![]),
        response_formats: None,
        supported_options: None,
    }
}

impl types::Host for State {}
impl run_types::Host for State {}
impl tool_types::Host for State {}

impl types::HostModel for State {
    fn drop(&mut self, rep: Resource<ModelHandle>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl<T: Send> types::HostModelWithStore<T> for HasSelf<State> {
    async fn describe(
        _store: &Accessor<T, Self>,
        _rep: Resource<ModelHandle>,
    ) -> wasmtime::Result<types::ModelDescription> {
        Ok(description())
    }
}

impl models::Host for State {}

impl<T: Send> models::HostWithStore<T> for HasSelf<State> {
    async fn open(
        store: &Accessor<T, Self>,
        alias: String,
    ) -> wasmtime::Result<Result<Resource<ModelHandle>, models::OpenError>> {
        store.with(|mut s| {
            if alias != s.get().host.alias {
                return Ok(Err(models::OpenError::UnknownAlias(alias)));
            }
            Ok(Ok(s.get().table.push(ModelHandle)?))
        })
    }

    async fn describe(
        store: &Accessor<T, Self>,
        alias: String,
    ) -> wasmtime::Result<Result<types::ModelDescription, models::OpenError>> {
        if store.with(|mut s| alias != s.get().host.alias) {
            return Ok(Err(models::OpenError::UnknownAlias(alias)));
        }
        Ok(Ok(description()))
    }
}

struct MessageCollector {
    messages: Vec<Message>,
    out: Option<oneshot::Sender<Vec<Message>>>,
}

impl Drop for MessageCollector {
    fn drop(&mut self) {
        if let Some(tx) = self.out.take() {
            let _ = tx.send(std::mem::take(&mut self.messages));
        }
    }
}

impl<D> StreamConsumer<D> for MessageCollector {
    type Item = Message;

    fn poll_consume(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        mut store: StoreContextMut<D>,
        mut src: Source<Self::Item>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let this = self.get_mut();
        while src.remaining(&mut store) > 0 {
            let mut one = None;
            src.read(&mut store, &mut one)?;
            this.messages.extend(one);
        }
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

struct DoneConsumer {
    out: Option<oneshot::Sender<Result<(), String>>>,
}

impl<D> FutureConsumer<D> for DoneConsumer {
    type Item = Result<(), String>;

    fn poll_consume(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        mut store: StoreContextMut<D>,
        mut src: Source<'_, Self::Item>,
        _finish: bool,
    ) -> Poll<wasmtime::Result<()>> {
        let mut one = None;
        src.read(&mut store, &mut one)?;
        if let Some(tx) = self.get_mut().out.take() {
            let _ = tx.send(one.unwrap_or(Ok(())));
        }
        Poll::Ready(Ok(()))
    }
}

struct ChunkProducer {
    chunks: mpsc::UnboundedReceiver<Chunk>,
}

impl<D: 'static> StreamProducer<D> for ChunkProducer {
    type Item = Chunk;
    type Buffer = Option<Chunk>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<'a, D>,
        mut dst: Destination<'a, Self::Item, Self::Buffer>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        if finish {
            return Poll::Ready(Ok(StreamResult::Cancelled));
        }
        if dst.remaining(&mut store) == Some(0) {
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        match self.get_mut().chunks.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => {
                dst.set_buffer(Some(chunk));
                Poll::Ready(Ok(StreamResult::Completed))
            }
            Poll::Ready(None) => Poll::Ready(Ok(StreamResult::Dropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn chunk(delta: ChunkDelta) -> Chunk {
    Chunk {
        delta,
        adaptations: Vec::new(),
        finish_reason: None,
        usage: None,
    }
}

fn part(content: AssistantContent) -> AssistantPart {
    AssistantPart {
        content,
        opaque: None,
        citations: Vec::new(),
    }
}

fn finish_reason(name: &str) -> FinishReason {
    match name {
        "stop" => FinishReason::Stop,
        "stop-sequence" => FinishReason::StopSequence,
        "max-tokens" => FinishReason::MaxTokens,
        "context-length" => FinishReason::ContextLength,
        "tool-calls" => FinishReason::ToolCalls,
        "refusal" => FinishReason::Refusal,
        "content-filter" => FinishReason::ContentFilter,
        "pause-turn" => FinishReason::PauseTurn,
        other => FinishReason::Other(other.into()),
    }
}

fn request_error(name: &str) -> RequestError {
    let detail = "scripted".to_string();
    match name {
        "rate-limited" => RequestError::RateLimited(types::RateLimit {
            retry_after_seconds: Some(1),
            detail,
        }),
        "context-length-exceeded" => RequestError::ContextLengthExceeded(types::ContextOverflow {
            prompt_tokens: None,
            context_length: None,
            detail,
        }),
        "timed-out" => RequestError::TimedOut,
        other => RequestError::Other(other.into()),
    }
}

/// The chunks and completion for one scripted reply.
fn reply(s: Scripted) -> Result<(Vec<Chunk>, Completion), RequestError> {
    let r = s.reply;
    if let Some(e) = &r.request_error {
        return Err(request_error(e));
    }
    let mut chunks = Vec::new();
    let mut parts = Vec::new();
    if let Some(t) = r.reasoning {
        chunks.push(chunk(ChunkDelta::ReasoningText(t)));
    }
    if let Some(t) = r.text {
        chunks.push(chunk(ChunkDelta::Text(t.clone())));
        parts.push(part(AssistantContent::Text(t)));
    }
    if let Some(t) = r.refusal {
        parts.push(part(AssistantContent::Refusal(t)));
    }
    for (call, id) in r.tool_calls.iter().zip(s.ids) {
        let arguments = match (&call.text, &call.arguments) {
            (Some(t), _) => ToolArguments::Text(t.clone()),
            (None, Some(v)) => ToolArguments::Json(v.to_string()),
            (None, None) => ToolArguments::Json("{}".into()),
        };
        parts.push(part(AssistantContent::ToolUse(ToolCall {
            id,
            name: call.name.clone(),
            arguments,
        })));
    }
    let finish = match (&r.finish, r.tool_calls.is_empty()) {
        (Some(f), _) => finish_reason(f),
        (None, false) => FinishReason::ToolCalls,
        (None, true) => FinishReason::Stop,
    };
    let completion = Completion {
        finish_reason: finish,
        usage: Usage {
            prompt_tokens: Some(100),
            cached_prompt_tokens: None,
            cache_write_tokens: None,
            completion_tokens: Some(10),
            reasoning_tokens: None,
        },
        message: AssistantMessage { content: parts },
        adaptations: Vec::new(),
        model: MODEL.into(),
    };
    Ok((chunks, completion))
}

impl chat::Host for State {}

impl<T: Send> chat::HostWithStore<T> for HasSelf<State> {
    async fn chat(
        store: &Accessor<T, Self>,
        _model: Resource<ModelHandle>,
        messages: StreamReader<Message>,
        messages_done: FutureReader<Result<(), String>>,
        options: types::ChatOptions,
    ) -> wasmtime::Result<
        Result<
            (
                StreamReader<Chunk>,
                FutureReader<Result<Completion, GenerationError>>,
            ),
            RequestError,
        >,
    > {
        let (messages_tx, messages_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let host = store.with(|mut s| -> wasmtime::Result<_> {
            let host = Arc::clone(&s.get().host);
            messages.pipe(
                &mut s,
                MessageCollector {
                    messages: Vec::new(),
                    out: Some(messages_tx),
                },
            )?;
            messages_done.pipe(&mut s, DoneConsumer { out: Some(done_tx) })?;
            Ok(host)
        })?;
        let _ = messages_rx.await;
        if let Ok(Err(why)) = done_rx.await {
            return Ok(Err(RequestError::MessagesIncomplete(why)));
        }
        let offered = options.tools.iter().map(|t| t.name.clone()).collect();
        let next = host.replies().pop_front();
        let calls = {
            let mut rec = host.record();
            rec.model_calls.push(offered);
            rec.model_calls.len()
        };
        let Some(next) = next else {
            host.record().problems.push(format!(
                "the agent called the model {calls} times; the script has {} replies",
                host.script_len
            ));
            return Ok(Err(RequestError::Other(
                "the scripted model has no more replies".into(),
            )));
        };
        let (chunks, completion) = match reply(next) {
            Ok(r) => r,
            Err(e) => return Ok(Err(e)),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        for c in chunks {
            let _ = tx.send(c);
        }
        drop(tx);
        store.with(|mut s| {
            let chunks = StreamReader::new(&mut s, ChunkProducer { chunks: rx })?;
            let outcome =
                FutureReader::new(
                    &mut s,
                    async move { Ok::<_, wasmtime::Error>(Ok(completion)) },
                )?;
            Ok(Ok((chunks, outcome)))
        })
    }
}
