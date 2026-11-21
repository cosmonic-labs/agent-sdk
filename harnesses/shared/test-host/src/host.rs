//! A scripted `cosmonic:agent@0.4.2` host: a model that answers from the
//! conversation it is sent, an in-memory session with the conditional commit
//! rules, and three tools. It drives any agent harness that exports `run`
//! and `compact`.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use wasmtime::component::{Accessor, HasSelf, Resource, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "wit-host",
    world: "agent-under-test",
    imports: { default: trappable },
    exports: { default: async },
    with: {
        "cosmonic:agent/inference-types@0.4.2.model": ModelHandle,
        "wasi:clocks": wasmtime_wasi::p3::bindings::clocks,
    },
    require_store_data_send: true,
});

pub use cosmonic::agent::{chat, inference_types as types, models, run_types, session, tool_types, tools};
use types::{
    AssistantContent, AssistantMessage, AssistantPart, Chunk, ChunkDelta, Completion, FinishReason, GenerationError,
    Message, RequestError, ToolCall, Usage,
};

pub const ALIAS: &str = "default";

/// A patch in Codex's apply_patch format: raw text, not JSON.
pub const PATCH: &str = "*** Begin Patch\n*** Update File: src/lib.rs\n@@ fn main\n-    println!(\"hi\");\n+    println!(\"hello\");\n*** End Patch\n";
/// The grammar the freeform `patch` tool declares.
pub const PATCH_GRAMMAR: &str = "start: \"*** Begin Patch\" LF hunk+ \"*** End Patch\" LF?";
pub const MODEL: &str = "scripted-model";

pub struct ModelHandle;

#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: u64,
    pub kind: String,
    pub data: String,
}

impl Entry {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.data).unwrap_or_default()
    }
}

#[derive(Clone)]
pub struct StoredState {
    pub value: Vec<u8>,
    pub version: String,
    pub through: Option<u64>,
}

/// One chat request as the scripted model received it.
#[derive(Clone, Debug)]
pub struct ChatRecord {
    pub messages: Vec<Message>,
    pub tools: Vec<String>,
    pub inputs: Vec<types::ToolInput>,
    pub response_format: types::ResponseFormat,
}

#[derive(Default)]
pub struct Agent {
    pub entries: Vec<Entry>,
    pub state: Option<StoredState>,
    pub seq: u64,
    pub versions: u64,
    pub commits: HashMap<String, session::CommitResult>,
    pub chats: Vec<ChatRecord>,
    pub tool_calls: Vec<ToolCall>,
    pub open_models: i64,
    /// What `models.describe` reports as the context window.
    pub context_length: Option<u32>,
    /// The journal's length each time the driver read a `plan` event.
    pub plan_events_at: Vec<usize>,
    /// How long each model call takes to complete, so a call can still be
    /// open when the agent handles what came before it.
    pub reply_delay_ms: u64,
    /// Faults the next call meets, each cleared once used:
    /// - another writer appends an entry after a model call;
    /// - a model call never finishes (`model_stalled` is then set);
    /// - a tool call's outcome is unknown, the provider lost after dispatch.
    pub stale_after_model: bool,
    pub stall_model: bool,
    pub model_stalled: bool,
    pub unknown_outcome: bool,
}

impl Agent {
    /// Append an entry outside any commit, as an interrupted writer leaves one.
    pub fn append(&mut self, kind: &str, data: serde_json::Value) {
        self.seq += 1;
        self.entries.push(Entry { seq: self.seq, kind: kind.into(), data: data.to_string() });
    }

    pub fn end(&self) -> session::JournalEnd {
        match self.entries.last() {
            Some(entry) => session::JournalEnd::At(entry.seq),
            None => session::JournalEnd::Empty,
        }
    }

    fn snapshot(&self) -> session::Snapshot {
        session::Snapshot {
            state: self.state.as_ref().map(|s| session::State {
                value: s.value.clone(),
                version: s.version.clone(),
                through: match s.through {
                    Some(seq) => session::JournalEnd::At(seq),
                    None => session::JournalEnd::Empty,
                },
            }),
            end: self.end(),
        }
    }

    fn commit(
        &mut self,
        entries: Vec<session::NewEntry>,
        state: Option<Vec<u8>>,
        options: session::CommitOptions,
    ) -> Result<session::CommitResult, session::Error> {
        if let Some(id) = &options.commit_id
            && let Some(result) = self.commits.get(id)
        {
            return Ok(result.clone());
        }
        if state.is_some() && (options.require_state.is_none() || options.require_end.is_none()) {
            return Err(session::Error::InvalidArgument("a state write requires both conditions".into()));
        }
        if let Some(end) = &options.require_end
            && !same_end(end, &self.end())
        {
            return Ok(session::CommitResult::Stale(self.snapshot()));
        }
        if let Some(condition) = &options.require_state {
            let current = self.state.as_ref().map(|s| s.version.as_str());
            let matches = match condition {
                session::StateCondition::Absent => current.is_none(),
                session::StateCondition::Version(v) => current == Some(v.as_str()),
            };
            if !matches {
                return Ok(session::CommitResult::Stale(self.snapshot()));
            }
        }
        for entry in entries {
            self.seq += 1;
            self.entries.push(Entry {
                seq: self.seq,
                kind: entry.kind,
                data: String::from_utf8_lossy(&entry.data).into_owned(),
            });
        }
        let mut version = None;
        if let Some(value) = state {
            self.versions += 1;
            let v = format!("v{}", self.versions);
            self.state = Some(StoredState {
                value,
                version: v.clone(),
                through: match self.end() {
                    session::JournalEnd::At(seq) => Some(seq),
                    session::JournalEnd::Empty => None,
                },
            });
            version = Some(v);
        }
        let result = session::CommitResult::Committed(session::Committed { end: self.end(), version });
        if let Some(id) = options.commit_id {
            self.commits.insert(id, result.clone());
        }
        Ok(result)
    }

    /// The saved Codex history, as JSON.
    pub fn history(&self) -> Vec<serde_json::Value> {
        let state: serde_json::Value =
            serde_json::from_slice(&self.state.as_ref().expect("no state").value).expect("state is JSON");
        assert_eq!(state["format"], "codex-slim/1");
        state["history"].as_array().cloned().unwrap_or_default()
    }

    pub fn kinds(&self, from: usize) -> Vec<String> {
        self.entries[from..].iter().map(|e| e.kind.clone()).collect()
    }
}

fn same_end(a: &session::JournalEnd, b: &session::JournalEnd) -> bool {
    match (a, b) {
        (session::JournalEnd::Empty, session::JournalEnd::Empty) => true,
        (session::JournalEnd::At(x), session::JournalEnd::At(y)) => x == y,
        _ => false,
    }
}

pub type Shared = Arc<Mutex<Agent>>;

pub struct State {
    pub table: ResourceTable,
    pub wasi: WasiCtx,
    pub http: wasmtime_wasi_http::WasiHttpCtx,
    pub hooks: NoRequests,
    pub agent: Shared,
}

impl State {
    pub fn new(agent: Shared) -> Self {
        State {
            table: ResourceTable::new(),
            wasi: wasmtime_wasi::WasiCtxBuilder::new().inherit_stderr().build(),
            http: wasmtime_wasi_http::WasiHttpCtx::new(),
            hooks: NoRequests,
            agent,
        }
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl wasmtime_wasi_http::WasiHttpView for State {
    fn http(&mut self) -> wasmtime_wasi_http::WasiHttpCtxView<'_> {
        wasmtime_wasi_http::WasiHttpCtxView { ctx: &mut self.http, table: &mut self.table, hooks: &mut self.hooks }
    }
}

/// A harness that exports an HTTP handler imports `wasi:http/types`; no agent
/// imports a client, and any request is refused.
pub struct NoRequests;

type HttpResult<T> = Result<T, wasmtime_wasi_http::Error>;
type HttpDone = Box<dyn Future<Output = HttpResult<()>> + Send>;

impl wasmtime_wasi_http::WasiHttpHooks for NoRequests {
    fn send_request(
        &mut self,
        _request: http::Request<wasmtime_wasi_http::WasiBody>,
        _options: Option<wasmtime_wasi_http::RequestOptions>,
        _completion: HttpDone,
    ) -> Box<dyn Future<Output = HttpResult<(http::Response<wasmtime_wasi_http::WasiBody>, HttpDone)>> + Send> {
        Box::new(async { Err(wasmtime_wasi_http::p3::bindings::http::types::ErrorCode::HttpRequestDenied.into()) })
    }
}

pub fn add_to_linker(linker: &mut wasmtime::component::Linker<State>) -> wasmtime::Result<()> {
    types::add_to_linker::<_, HasSelf<_>>(linker, |s| s)?;
    models::add_to_linker::<_, HasSelf<_>>(linker, |s| s)?;
    chat::add_to_linker::<_, HasSelf<_>>(linker, |s| s)?;
    session::add_to_linker::<_, HasSelf<_>>(linker, |s| s)?;
    tool_types::add_to_linker::<_, HasSelf<_>>(linker, |s| s)?;
    tools::add_to_linker::<_, HasSelf<_>>(linker, |s| s)?;
    run_types::add_to_linker::<_, HasSelf<_>>(linker, |s| s)?;
    Ok(())
}

fn description(context_length: Option<u32>) -> types::ModelDescription {
    types::ModelDescription {
        model: Some(MODEL.into()),
        supported_operations: types::SupportedOperations::CHAT,
        context_length,
        max_output_tokens: Some(1024),
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
        self.agent.lock().unwrap().open_models -= 1;
        Ok(())
    }
}

impl<T: Send> types::HostModelWithStore<T> for HasSelf<State> {
    async fn describe(store: &Accessor<T, Self>, _rep: Resource<ModelHandle>) -> wasmtime::Result<types::ModelDescription> {
        Ok(description(store.with(|mut s| s.get().agent.lock().unwrap().context_length)))
    }
}

impl models::Host for State {}

impl<T: Send> models::HostWithStore<T> for HasSelf<State> {
    async fn open(
        store: &Accessor<T, Self>,
        alias: String,
    ) -> wasmtime::Result<Result<Resource<ModelHandle>, models::OpenError>> {
        if alias != ALIAS {
            return Ok(Err(models::OpenError::UnknownAlias(alias)));
        }
        store.with(|mut s| {
            s.get().agent.lock().unwrap().open_models += 1;
            Ok(Ok(s.get().table.push(ModelHandle)?))
        })
    }

    async fn describe(
        store: &Accessor<T, Self>,
        alias: String,
    ) -> wasmtime::Result<Result<types::ModelDescription, models::OpenError>> {
        if alias != ALIAS {
            return Ok(Err(models::OpenError::UnknownAlias(alias)));
        }
        Ok(Ok(description(store.with(|mut s| s.get().agent.lock().unwrap().context_length))))
    }
}

// ----- chat: the scripted model -----

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

impl<D> wasmtime::component::StreamConsumer<D> for MessageCollector {
    type Item = Message;

    fn poll_consume(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        mut store: wasmtime::StoreContextMut<D>,
        mut src: wasmtime::component::Source<Self::Item>,
        _finish: bool,
    ) -> std::task::Poll<wasmtime::Result<wasmtime::component::StreamResult>> {
        let this = self.get_mut();
        while src.remaining(&mut store) > 0 {
            let mut one = None;
            src.read(&mut store, &mut one)?;
            this.messages.extend(one);
        }
        std::task::Poll::Ready(Ok(wasmtime::component::StreamResult::Completed))
    }
}

struct DoneConsumer {
    out: Option<oneshot::Sender<Result<(), String>>>,
}

impl<D> wasmtime::component::FutureConsumer<D> for DoneConsumer {
    type Item = Result<(), String>;

    fn poll_consume(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        mut store: wasmtime::StoreContextMut<D>,
        mut src: wasmtime::component::Source<'_, Self::Item>,
        _finish: bool,
    ) -> std::task::Poll<wasmtime::Result<()>> {
        let mut one = None;
        src.read(&mut store, &mut one)?;
        if let Some(tx) = self.get_mut().out.take() {
            let _ = tx.send(one.unwrap_or(Ok(())));
        }
        std::task::Poll::Ready(Ok(()))
    }
}

struct ChunkProducer {
    chunks: mpsc::UnboundedReceiver<Chunk>,
}

impl<D: 'static> wasmtime::component::StreamProducer<D> for ChunkProducer {
    type Item = Chunk;
    type Buffer = Option<Chunk>;

    fn poll_produce<'a>(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut store: wasmtime::StoreContextMut<'a, D>,
        mut dst: wasmtime::component::Destination<'a, Self::Item, Self::Buffer>,
        finish: bool,
    ) -> std::task::Poll<wasmtime::Result<wasmtime::component::StreamResult>> {
        use std::task::Poll;
        use wasmtime::component::StreamResult;
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

pub fn chunk(delta: ChunkDelta) -> Chunk {
    Chunk { delta, adaptations: Vec::new(), finish_reason: None, usage: None }
}

pub fn part(content: AssistantContent, opaque: Option<&str>) -> AssistantPart {
    AssistantPart { content, opaque: opaque.map(str::to_string), citations: Vec::new() }
}

pub fn completion(parts: Vec<AssistantPart>, finish: FinishReason) -> Completion {
    Completion {
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
    }
}

pub enum Script {
    Refuse(RequestError),
    Reply { chunks: Vec<Chunk>, outcome: Result<Completion, GenerationError> },
}

pub fn user_text(message: &Message) -> Option<String> {
    match message {
        Message::User(user) => Some(
            user.content
                .iter()
                .filter_map(|part| match part {
                    types::UserPart::Text(text) => Some(text.as_str()),
                    types::UserPart::Attachment(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// Codex's compaction prompt, sent as a user message.
pub const SUMMARIZATION_MARKER: &str = "CONTEXT CHECKPOINT COMPACTION";
/// How Pi's summarizer instructions start, sent as the system message; the
/// transcript to summarize is the first user message.
pub const PI_SUMMARY_SYSTEM: &str = "You are summarizing the earlier part of a working session";

/// The index of the message that asks for a summary, if `messages` is a
/// summary request from any of the harnesses. The person's focus, when
/// there is one, is the user message right after it.
pub fn summary_request(messages: &[Message]) -> Option<usize> {
    if let Some(at) = messages
        .iter()
        .rposition(|m| user_text(m).is_some_and(|t| t.contains(SUMMARIZATION_MARKER)))
    {
        return Some(at);
    }
    if messages.iter().any(|m| matches!(m, Message::System(s) if s.starts_with(PI_SUMMARY_SYSTEM))) {
        return messages.iter().position(|m| matches!(m, Message::User(_)));
    }
    None
}
/// A focus that makes the scripted model refuse to summarize.
pub const REFUSE_SUMMARY: &str = "refuse the summary";

/// The scripted model. Decides from the last message:
/// - a tool result: report it back as text;
/// - the compaction prompt: a summary;
/// - "echo X": reasoning (with continuation data), a text part, and a call to
///   echo (with continuation data);
/// - "call missing": a call to a tool the host does not offer;
/// - "plan": a call to the agent's own `update_plan`;
/// - "patch": a call to the freeform `patch` tool with raw patch text;
/// - anything else: a two-chunk text reply.
fn script(messages: &[Message]) -> Script {
    let last = messages.last();
    if let Some(Message::Tool(results)) = last {
        let text = results
            .iter()
            .flat_map(|r| r.output.iter())
            .filter_map(|o| match o {
                types::ToolOutput::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let reply = format!("The tool said: {text}");
        return Script::Reply {
            chunks: vec![chunk(ChunkDelta::Text(reply.clone()))],
            outcome: Ok(completion(vec![part(AssistantContent::Text(reply), None)], FinishReason::Stop)),
        };
    }
    let text = last.and_then(user_text).unwrap_or_default();
    // A summary request: the compaction prompt as a user message, last or
    // followed by the person's focus as their own message.
    if let Some(at) = summary_request(messages) {
        let focus = messages.get(at + 1).and_then(user_text);
        if focus.as_deref() == Some(REFUSE_SUMMARY) {
            let refusal = "I won't summarize this.".to_string();
            return Script::Reply {
                chunks: vec![chunk(ChunkDelta::Text(refusal.clone()))],
                outcome: Ok(completion(vec![part(AssistantContent::Refusal(refusal), None)], FinishReason::Refusal)),
            };
        }
        let mut summary = "SUMMARY: the person asked about echo and padding.".to_string();
        if let Some(focus) = focus {
            summary.push_str(&format!(" FOCUS: {focus}"));
        }
        return Script::Reply {
            chunks: vec![chunk(ChunkDelta::Text(summary.clone()))],
            outcome: Ok(completion(vec![part(AssistantContent::Text(summary), None)], FinishReason::Stop)),
        };
    }
    let tool_reply = |name: &str, arguments: types::ToolArguments| {
        let call = ToolCall { id: "call-1".into(), name: name.into(), arguments };
        Script::Reply {
            chunks: vec![
                chunk(ChunkDelta::ReasoningText("Planning the call.".into())),
                chunk(ChunkDelta::Text("Using the tool.".into())),
            ],
            outcome: Ok(completion(
                vec![
                    part(AssistantContent::Reasoning(types::Reasoning { summary: vec!["Planning the call.".into()] }), Some("enc-1")),
                    part(AssistantContent::Text("Using the tool.".into()), None),
                    part(AssistantContent::ToolUse(call), Some("sig-1")),
                ],
                FinishReason::ToolCalls,
            )),
        }
    };
    if let Some(rest) = text.strip_prefix("echo ") {
        return tool_reply("echo", types::ToolArguments::Json(serde_json::json!({"text": rest}).to_string()));
    }
    if text.starts_with("call missing") {
        return tool_reply("missing", types::ToolArguments::Json("{}".into()));
    }
    if text.starts_with("patch") {
        return tool_reply("patch", types::ToolArguments::Text(PATCH.into()));
    }
    if text.starts_with("plan") {
        let plan = serde_json::json!({
            "explanation": "Three steps.",
            "plan": [
                {"step": "Find the handler", "status": "completed"},
                {"step": "Fix the bug", "status": "in_progress"},
                {"step": "Run the tests", "status": "pending"},
            ],
        });
        return tool_reply("update_plan", types::ToolArguments::Json(plan.to_string()));
    }
    if text.starts_with("refuse") {
        return Script::Refuse(RequestError::RateLimited(types::RateLimit {
            retry_after_seconds: Some(1),
            detail: "scripted refusal".into(),
        }));
    }
    // "overflow": too long for the window until the context holds a summary.
    if text.starts_with("overflow") && !messages.iter().any(|m| format!("{m:?}").contains("SUMMARY:")) {
        return Script::Refuse(RequestError::ContextLengthExceeded(types::ContextOverflow {
            prompt_tokens: Some(200),
            context_length: Some(100),
            detail: "scripted overflow".into(),
        }));
    }
    let reply = format!("Reply to: {text}");
    let (a, b) = reply.split_at(reply.len() / 2);
    Script::Reply {
        chunks: vec![chunk(ChunkDelta::Text(a.into())), chunk(ChunkDelta::Text(b.into()))],
        outcome: Ok(completion(vec![part(AssistantContent::Text(reply.clone()), None)], FinishReason::Stop)),
    }
}

impl chat::Host for State {}

impl<T: Send> chat::HostWithStore<T> for HasSelf<State> {
    async fn chat(
        store: &Accessor<T, Self>,
        _model: Resource<ModelHandle>,
        messages: wasmtime::component::StreamReader<Message>,
        messages_done: wasmtime::component::FutureReader<Result<(), String>>,
        options: types::ChatOptions,
    ) -> wasmtime::Result<
        Result<
            (
                wasmtime::component::StreamReader<Chunk>,
                wasmtime::component::FutureReader<Result<Completion, GenerationError>>,
            ),
            RequestError,
        >,
    > {
        let (messages_tx, messages_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        let agent = store.with(|mut s| -> wasmtime::Result<_> {
            let agent = Arc::clone(&s.get().agent);
            messages.pipe(&mut s, MessageCollector { messages: Vec::new(), out: Some(messages_tx) })?;
            messages_done.pipe(&mut s, DoneConsumer { out: Some(done_tx) })?;
            Ok(agent)
        })?;
        let messages = messages_rx.await.unwrap_or_default();
        if let Ok(Err(why)) = done_rx.await {
            return Ok(Err(RequestError::MessagesIncomplete(why)));
        }
        let (delay, stall) = {
            let mut agent = agent.lock().unwrap();
            agent.chats.push(ChatRecord {
                messages: messages.clone(),
                tools: options.tools.iter().map(|t| t.name.clone()).collect(),
                inputs: options.tools.iter().map(|t| t.input.clone()).collect(),
                response_format: options.response_format.clone(),
            });
            let stall = std::mem::take(&mut agent.stall_model);
            agent.model_stalled |= stall;
            if !stall && std::mem::take(&mut agent.stale_after_model) {
                agent.append("agent.step.v1", serde_json::json!({"role": "assistant", "text": "concurrent write"}));
            }
            (std::time::Duration::from_millis(agent.reply_delay_ms), stall)
        };
        if stall {
            std::future::pending::<()>().await;
        }
        let (chunks, outcome) = match script(&messages) {
            Script::Refuse(error) => return Ok(Err(error)),
            Script::Reply { chunks, outcome } => (chunks, outcome),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        for c in chunks {
            let _ = tx.send(c);
        }
        drop(tx);
        store.with(|mut s| {
            let chunks = wasmtime::component::StreamReader::new(&mut s, ChunkProducer { chunks: rx })?;
            let outcome = wasmtime::component::FutureReader::new(&mut s, async move {
                tokio::time::sleep(delay).await;
                Ok::<_, wasmtime::Error>(outcome)
            })?;
            Ok(Ok((chunks, outcome)))
        })
    }
}

// ----- session -----

impl session::Host for State {}

impl<T: Send> session::HostWithStore<T> for HasSelf<State> {
    async fn current(store: &Accessor<T, Self>) -> wasmtime::Result<Result<session::Snapshot, session::Error>> {
        Ok(Ok(store.with(|mut s| s.get().agent.lock().unwrap().snapshot())))
    }

    async fn commit(
        store: &Accessor<T, Self>,
        entries: Vec<session::NewEntry>,
        state: Option<Vec<u8>>,
        options: session::CommitOptions,
    ) -> wasmtime::Result<Result<session::CommitResult, session::Error>> {
        Ok(store.with(|mut s| s.get().agent.lock().unwrap().commit(entries, state, options)))
    }

    async fn read(
        store: &Accessor<T, Self>,
        start: u64,
        max: u32,
    ) -> wasmtime::Result<Result<Vec<session::Entry>, session::Error>> {
        Ok(Ok(store.with(|mut s| {
            s.get()
                .agent
                .lock()
                .unwrap()
                .entries
                .iter()
                .filter(|e| e.seq >= start)
                .take(max as usize)
                .map(|e| session::Entry {
                    seq: e.seq,
                    author: "agent".into(),
                    at_ms: 0,
                    kind: e.kind.clone(),
                    data: e.data.clone().into_bytes(),
                })
                .collect()
        })))
    }

    async fn follow(
        _store: &Accessor<T, Self>,
        _start: u64,
    ) -> wasmtime::Result<Result<wasmtime::component::StreamReader<session::Entry>, session::Error>> {
        Ok(Err(session::Error::Unavailable("this host does not follow".into())))
    }

    async fn clear(store: &Accessor<T, Self>) -> wasmtime::Result<Result<(), session::Error>> {
        store.with(|mut s| {
            let mut agent = s.get().agent.lock().unwrap();
            agent.entries.clear();
            agent.state = None;
            agent.commits.clear();
        });
        Ok(Ok(()))
    }
}

// ----- tools: echo, one that fails, and a freeform patch tool -----

impl tools::Host for State {}

fn tool_def(name: &str, description: &str, schema: &str) -> tools::ToolDef {
    tool_def_as(name, description, types::ToolInput::JsonSchema(schema.into()))
}

fn tool_def_as(name: &str, description: &str, input: types::ToolInput) -> tools::ToolDef {
    tool_types::ToolDef {
        name: name.into(),
        title: None,
        description: description.into(),
        input,
        output_schema: None,
        annotations: tool_types::ToolAnnotations {
            read_only: Some(true),
            destructive: Some(false),
            idempotent: Some(true),
            open_world: Some(false),
        },
        version: None,
    }
}

impl<T: Send> tools::HostWithStore<T> for HasSelf<State> {
    async fn list(_store: &Accessor<T, Self>) -> wasmtime::Result<Result<Vec<tools::ToolDef>, tools::ListError>> {
        Ok(Ok(vec![
            tool_def(
                "echo",
                "Echo the text back",
                r#"{"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}"#,
            ),
            tool_def("explode", "Always fails", r#"{"type": "object"}"#),
            tool_def_as(
                "patch",
                "Apply a patch",
                types::ToolInput::Grammar(types::Grammar {
                    syntax: types::GrammarSyntax::Lark,
                    definition: PATCH_GRAMMAR.into(),
                }),
            ),
        ]))
    }

    async fn call(
        store: &Accessor<T, Self>,
        request: ToolCall,
    ) -> wasmtime::Result<Result<tools::ToolResponse, tools::CallError>> {
        let unknown = store.with(|mut s| {
            let mut agent = s.get().agent.lock().unwrap();
            agent.tool_calls.push(request.clone());
            std::mem::take(&mut agent.unknown_outcome)
        });
        if unknown {
            return Ok(Err(tool_types::CallError::OutcomeUnknown(tool_types::OutcomeError::Upstream(
                "the provider went away".into(),
            ))));
        }
        let response = match (request.name.as_str(), &request.arguments) {
            ("echo", types::ToolArguments::Json(json)) => {
                let arguments: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
                tool_types::ToolResponse {
                    output: vec![types::ToolOutput::Text(arguments["text"].as_str().unwrap_or_default().to_string())],
                    structured: None,
                    is_error: false,
                }
            }
            ("patch", types::ToolArguments::Text(text)) => tool_types::ToolResponse {
                output: vec![types::ToolOutput::Text(format!("applied {} bytes", text.len()))],
                structured: None,
                is_error: false,
            },
            ("echo" | "patch", other) => {
                return Ok(Err(tool_types::CallError::NotDispatched(tool_types::DispatchError::InvalidArguments(
                    format!("wrong kind of arguments: {other:?}"),
                ))));
            }
            ("explode", _) => tool_types::ToolResponse {
                output: vec![types::ToolOutput::Text("the tool exploded".into())],
                structured: None,
                is_error: true,
            },
            (other, _) => {
                return Ok(Err(tool_types::CallError::NotDispatched(tool_types::DispatchError::UnknownTool(
                    other.to_string(),
                ))));
            }
        };
        Ok(Ok(response))
    }
}
