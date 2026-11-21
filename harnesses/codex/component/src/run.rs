//! The typed `cosmonic:agent/run@0.4.2` export: validate the request, open
//! the run, then work while the driver reads the events, and resolve the
//! outcome after the last commit.

use wit_bindgen::rt::async_support::StreamWriter;

use crate::bindings::cosmonic::agent::inference_types as llm;
use crate::bindings::cosmonic::agent::run_types::{
    CitedReplyText, CompactionInfo, Finish as RunFinish, PlanEntry, ReplyPart, RunCounts, RunError, RunEvent, RunOptions, RunOutcome, ServedModel, StartError,
    StartedRun, Step, StepContent,
};
use crate::bindings::exports::cosmonic::agent::compact::Guest as CompactGuest;
use crate::bindings::exports::cosmonic::agent::run::Guest;
use crate::bindings::{wit_future, wit_stream};
use crate::host::{Deltas, SampleError, MODEL_ALIAS};
use crate::session::TurnError;
use crate::turn::{Counts, Ended, Finish, Sink, TurnRequest};
use crate::{DEFAULT_MAX_STEPS, MAX_STEPS_CAP};

/// The commit id `run:<run-id>` must fit the session's 128-byte limit.
const MAX_RUN_ID_BYTES: usize = 124;

pub struct Runner;

impl Guest for Runner {
    async fn run(message: llm::UserMessage, options: RunOptions) -> Result<StartedRun, StartError> {
        let req = validate(message, options).map_err(StartError::InvalidInput)?;
        let (acc, running) = crate::accept(&req).await.map_err(|e| match e {
            TurnError::Busy => StartError::Busy,
            TurnError::Duplicate => StartError::Duplicate,
            e => StartError::Unavailable(format!("the session store failed: {e}")),
        })?;
        let (events_tx, events_rx) = wit_stream::new::<RunEvent>();
        let (outcome_tx, outcome_rx) = wit_future::new::<Result<RunOutcome, RunError>>(|| {
            Err(RunError::Failed("the agent stopped before the run ended".into()))
        });
        wasip3::spawn_local(async move {
            let _running = running;
            let mut events = Events(events_tx);
            let ended = crate::turn::work(&mut events, &req, acc).await;
            // The stream closes before the outcome resolves.
            drop(events);
            let _ = outcome_tx.write(outcome(ended)).await;
        });
        Ok((events_rx, outcome_rx))
    }
}

impl CompactGuest for Runner {
    async fn compact(focus: Option<String>, options: RunOptions) -> Result<StartedRun, StartError> {
        crate::requested::start(focus, options).await
    }
}

fn validate(message: llm::UserMessage, options: RunOptions) -> Result<TurnRequest, String> {
    let run = options.run_id;
    if run.is_empty() || run.len() > MAX_RUN_ID_BYTES {
        return Err(format!("a run id is 1 to {MAX_RUN_ID_BYTES} bytes"));
    }
    if run.chars().any(char::is_control) {
        return Err("a run id has no control characters".into());
    }
    let mut parts = Vec::new();
    for part in message.content {
        match part {
            llm::UserPart::Text(t) => parts.push(t),
            llm::UserPart::Attachment(a) => {
                return Err(format!("this agent takes text, not a {} attachment", a.media_type));
            }
        }
    }
    let text = parts.join("\n");
    if text.trim().is_empty() {
        return Err("the message has no text".into());
    }
    let max_steps = match options.max_steps {
        None => DEFAULT_MAX_STEPS,
        Some(0) => return Err("max-steps is at least 1".into()),
        Some(n) => n.min(MAX_STEPS_CAP),
    };
    if let Some(schema) = &options.schema {
        if serde_json::from_str::<serde_json::Value>(schema).is_err() {
            return Err("the schema is not JSON".into());
        }
    }
    Ok(TurnRequest {
        run,
        text,
        instructions: options.instructions.filter(|i| !i.trim().is_empty()),
        schema: options.schema,
        max_steps,
        max_total_tokens: options.max_total_tokens,
        max_duration: options.max_duration,
        effort: options.effort,
        trace_parent: options.trace_parent,
    })
}

struct Events(StreamWriter<RunEvent>);

impl Events {
    async fn put(&mut self, event: RunEvent) -> bool {
        self.0.write_one(event).await.is_none()
    }
}

impl Deltas for Events {
    async fn text(&mut self, text: &str) -> bool {
        self.put(RunEvent::TextDelta(text.to_string())).await
    }
    async fn reasoning(&mut self, text: &str) -> bool {
        self.put(RunEvent::ReasoningDelta(text.to_string())).await
    }
}

impl Sink for Events {
    async fn assistant(&mut self, message: llm::AssistantMessage) -> bool {
        self.put(RunEvent::Step(Step { content: StepContent::Assistant(message), seq: None })).await
    }
    async fn tool_results(&mut self, results: Vec<llm::ToolResult>) -> bool {
        self.put(RunEvent::Step(Step { content: StepContent::Tool(results), seq: None })).await
    }
    async fn error(&mut self, text: &str) -> bool {
        self.put(RunEvent::Step(Step { content: StepContent::Error(text.to_string()), seq: None })).await
    }
    async fn counts(&mut self, counts: &Counts) -> bool {
        self.put(RunEvent::Counts(run_counts(counts))).await
    }
    async fn plan(&mut self, plan: Vec<PlanEntry>) -> bool {
        self.put(RunEvent::Plan(plan)).await
    }
    async fn compacted(&mut self, info: CompactionInfo) -> bool {
        self.put(RunEvent::Compacted(info)).await
    }
}

fn run_counts(counts: &Counts) -> RunCounts {
    RunCounts {
        steps: counts.steps,
        tool_calls: counts.tool_calls,
        usage: llm::Usage {
            prompt_tokens: counts.prompt.map(|t| u32::try_from(t).unwrap_or(u32::MAX)),
            cached_prompt_tokens: None,
            cache_write_tokens: None,
            completion_tokens: counts.completion.map(|t| u32::try_from(t).unwrap_or(u32::MAX)),
            reasoning_tokens: None,
        },
    }
}

fn outcome(ended: Ended) -> Result<RunOutcome, RunError> {
    match ended {
        Ended::Done(f) => Ok(RunOutcome {
            reply: f
                .reply
                .into_iter()
                .filter_map(|p| match p.content {
                    llm::AssistantContent::Text(t) if p.citations.is_empty() => Some(ReplyPart::Text(t)),
                    llm::AssistantContent::Text(text) => {
                        Some(ReplyPart::CitedText(CitedReplyText { text, citations: p.citations }))
                    }
                    llm::AssistantContent::Refusal(t) => Some(ReplyPart::Refusal(t)),
                    llm::AssistantContent::Attachment(a) => Some(ReplyPart::Attachment(a)),
                    llm::AssistantContent::Other(b) => Some(ReplyPart::Opaque(b)),
                    llm::AssistantContent::Reasoning(_) | llm::AssistantContent::ToolUse(_) => None,
                })
                .collect(),
            structured: f.structured,
            counts: run_counts(&f.counts),
            finish: match f.finish {
                Finish::Done => RunFinish::Done,
                Finish::MaxSteps => RunFinish::MaxSteps,
                Finish::MaxTotalTokens => RunFinish::MaxTotalTokens,
                Finish::MaxDuration => RunFinish::MaxDuration,
                Finish::Refusal => RunFinish::Refusal,
                Finish::ContentFilter => RunFinish::ContentFilter,
            },
            model: f.model.map(|model| ServedModel { alias: MODEL_ALIAS.to_string(), model }),
        }),
        Ended::Cancelled => Err(RunError::Cancelled),
        Ended::Failed(f) => Err(match (f.case, f.host) {
            ("superseded", _) => RunError::Superseded,
            ("not-recorded", _) => RunError::NotRecorded(f.message),
            ("output-rejected", _) => RunError::OutputRejected(f.message),
            (_, Some(SampleError::Open(m))) => RunError::NoModel(m),
            (_, Some(SampleError::Refused(e))) => RunError::ModelRefused(e),
            (_, Some(SampleError::Failed(e))) => RunError::ModelFailed(e),
            _ => RunError::Failed(f.message),
        }),
    }
}
