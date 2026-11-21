//! The `cosmonic:agent/compact@0.4.2` export: summarize the history now, at
//! the driver's request, between runs.
//!
//! It shares the run lock (`busy` while a run or another compaction is in
//! progress) and makes Codex's own summary call (`turn::summary_call`), with
//! the person's `focus`, when given, as their own message after the prompt.
//! It keeps what the automatic compaction keeps: the person's recent
//! messages, then the summary.
//!
//! Journal, as the interface describes it:
//!
//! ```text
//! accept:  [unresolved ops closed, compact: the id and focus]   (commit id compact:<id>)
//! begin:   [op: target model]                                  (no state)
//!          ... the summary call ...
//! install: [op-result, compaction, compact-result] + state      (one commit)
//! ```
//!
//! Every other ending writes `agent.compact-result.v1` alone, with
//! `compacted` false: done when there was nothing to summarize or a limit
//! stopped it before the call, failed otherwise (in the same commit as the
//! summary call's op result, failed, when the call was made). The state is
//! left as it was. A refused or filtered summary is never committed.

use std::pin::pin;

use futures::future::{select, Either};
use serde_json::json;
use wit_bindgen::rt::async_support::StreamWriter;

use crate::bindings::cosmonic::agent::inference_types as llm;
use crate::bindings::cosmonic::agent::run_types::{
    CompactionInfo, CompactionTrigger, Finish as RunFinish, RunCounts, RunError, RunEvent, RunOptions, RunOutcome,
    SeqRange, ServedModel, StartError, StartedRun,
};
use crate::bindings::{wit_future, wit_stream};
use crate::codex::history::ContextManager;
use crate::codex::models::ResponseItem;
use crate::codex::BASE_INSTRUCTIONS;
use crate::host::{SampleError, MODEL_ALIAS};
use crate::session::{self, compact_result, Compacted, Journal, OpenedCompaction, TurnError};
use crate::turn::{self, Counts, Finish};
use crate::{Running, MAX_STEPS_CAP};

/// The commit id `compact:<id>` must fit the session's 128-byte limit.
const MAX_COMPACT_ID_BYTES: usize = 120;
/// The longest focus taken, in characters: the interface's recommended cap.
const MAX_FOCUS_CHARS: usize = 2000;

/// What a driver asked for, already validated.
pub struct CompactRequest {
    pub id: String,
    pub focus: Option<String>,
    pub instructions: Option<String>,
    pub max_steps: u32,
    pub max_total_tokens: Option<u64>,
    pub max_duration: Option<u64>,
    pub effort: Option<llm::Effort>,
    pub trace_parent: Option<String>,
}

/// Check what the driver sent, before anything is written. `schema` does not
/// apply to a compaction and is ignored.
pub fn validate(focus: Option<String>, options: RunOptions) -> Result<CompactRequest, String> {
    let id = options.run_id;
    if id.is_empty() || id.len() > MAX_COMPACT_ID_BYTES {
        return Err(format!("a compaction id is 1 to {MAX_COMPACT_ID_BYTES} bytes"));
    }
    if id.chars().any(char::is_control) {
        return Err("a compaction id has no control characters".into());
    }
    let focus = focus.filter(|f| !f.trim().is_empty());
    if focus.as_ref().is_some_and(|f| f.chars().count() > MAX_FOCUS_CHARS) {
        return Err(format!("a focus is at most {MAX_FOCUS_CHARS} characters"));
    }
    let max_steps = match options.max_steps {
        Some(0) => return Err("max-steps is at least 1".into()),
        Some(n) => n.min(MAX_STEPS_CAP),
        None => 1,
    };
    Ok(CompactRequest {
        id,
        focus,
        instructions: options.instructions.filter(|i| !i.trim().is_empty()),
        max_steps,
        max_total_tokens: options.max_total_tokens,
        max_duration: options.max_duration,
        effort: options.effort,
        trace_parent: options.trace_parent,
    })
}

/// Start one compaction: take the lock, accept it, and work while the driver
/// reads the events.
pub async fn start(focus: Option<String>, options: RunOptions) -> Result<StartedRun, StartError> {
    let req = validate(focus, options).map_err(StartError::InvalidInput)?;
    let running = crate::RUNNING.try_lock().ok_or(StartError::Busy)?;
    let opened = session::open_compaction(&req.id, req.focus.as_deref()).await.map_err(|e| match e {
        TurnError::Busy => StartError::Busy,
        TurnError::Duplicate => StartError::Duplicate,
        e => StartError::Unavailable(format!("the session store failed: {e}")),
    })?;
    let deadline = req.max_duration.map(|d| wasip3::clocks::monotonic_clock::now().saturating_add(d));
    let (events_tx, events_rx) = wit_stream::new::<RunEvent>();
    let (outcome_tx, outcome_rx) = wit_future::new::<Result<RunOutcome, RunError>>(|| {
        Err(RunError::Failed("the agent stopped before the compaction ended".into()))
    });
    wasip3::spawn_local(async move {
        let _running: Running = running;
        let mut events = Events(events_tx);
        let outcome = work(&mut events, &req, opened, deadline).await;
        // The stream closes before the outcome resolves.
        drop(events);
        let _ = outcome_tx.write(outcome).await;
    });
    Ok((events_rx, outcome_rx))
}

struct Events(StreamWriter<RunEvent>);

impl Events {
    async fn put(&mut self, event: RunEvent) -> bool {
        self.0.write_one(event).await.is_none()
    }

    /// Resolves once the driver has dropped the events.
    /// A dropped reader shows only on a write,
    /// so each quiet second writes an `other` event, which drivers ignore.
    async fn dropped(&mut self) {
        loop {
            wasip3::clocks::monotonic_clock::wait_for(HEARTBEAT_NS).await;
            if !self.put(RunEvent::Other("heartbeat".into())).await {
                return;
            }
        }
    }
}

/// How long the events may stay quiet before a heartbeat: one second.
const HEARTBEAT_NS: u64 = 1_000_000_000;

/// Whether a history holds anything a summary would replace: model output
/// (replies, calls, their results) since the last summary. Codex rebuilds the
/// history from the person's messages and the summary, so a history of only
/// those would come back as it went.
pub fn summarizable(history: &[ResponseItem]) -> bool {
    history.iter().any(|item| !matches!(item, ResponseItem::Message { role, .. } if role == "user"))
}

fn counts_of(counts: &Counts) -> RunCounts {
    RunCounts {
        steps: counts.steps,
        tool_calls: 0,
        usage: llm::Usage {
            prompt_tokens: counts.prompt.map(|t| u32::try_from(t).unwrap_or(u32::MAX)),
            cached_prompt_tokens: None,
            cache_write_tokens: None,
            completion_tokens: counts.completion.map(|t| u32::try_from(t).unwrap_or(u32::MAX)),
            reasoning_tokens: None,
        },
    }
}

fn done(counts: &Counts, finish: RunFinish, model: Option<String>) -> RunOutcome {
    RunOutcome {
        reply: Vec::new(),
        structured: None,
        counts: counts_of(counts),
        finish,
        model: model.map(|model| ServedModel { alias: MODEL_ALIAS.to_string(), model }),
    }
}

/// End without a summary: `agent.compact-result.v1` (after the failed result
/// of the summary call's `op`, when it was made), then the outcome.
async fn ended(
    journal: &Journal,
    id: &str,
    op: Option<&str>,
    outcome: Result<RunOutcome, (&'static str, RunError)>,
) -> Result<RunOutcome, RunError> {
    match outcome {
        Ok(out) => {
            let finish = match out.finish {
                RunFinish::MaxSteps => "max-steps",
                RunFinish::MaxTotalTokens => "max-total-tokens",
                RunFinish::MaxDuration => "max-duration",
                _ => "done",
            };
            journal.end_compaction(op, compact_result(id, "done", Some(finish), None, false)).await;
            Ok(out)
        }
        Err((case, error)) => {
            let outcome = if case == "cancelled" { "cancelled" } else { "failed" };
            journal.end_compaction(op, compact_result(id, outcome, None, Some(case), false)).await;
            Err(error)
        }
    }
}

/// The limit a compaction has reached before its summary call, if any.
fn limit_before(req: &CompactRequest, counts: &Counts, deadline: Option<u64>) -> Option<RunFinish> {
    if req.max_total_tokens.is_some_and(|m| counts.total() >= m) {
        Some(RunFinish::MaxTotalTokens)
    } else if deadline.is_some_and(|d| wasip3::clocks::monotonic_clock::now() >= d) {
        Some(RunFinish::MaxDuration)
    } else if counts.steps >= req.max_steps {
        Some(RunFinish::MaxSteps)
    } else {
        None
    }
}

async fn work(
    events: &mut Events,
    req: &CompactRequest,
    opened: OpenedCompaction,
    deadline: Option<u64>,
) -> Result<RunOutcome, RunError> {
    let OpenedCompaction { journal, conversation, covered } = opened;
    let id = req.id.as_str();
    let mut counts = Counts::default();
    let history = match conversation {
        Some(history) if summarizable(&history) => history,
        // Nothing to summarize: done, with no event and no model call.
        _ => return ended(&journal, id, None, Ok(done(&counts, RunFinish::Done, None))).await,
    };
    if let Some(finish) = limit_before(req, &counts, deadline) {
        return ended(&journal, id, None, Ok(done(&counts, finish, None))).await;
    }

    let mut system = Vec::new();
    system.extend(req.instructions.clone());
    system.push(BASE_INSTRUCTIONS.to_string());
    let history = ContextManager::new(history);
    let before = history.estimate_token_count(&system.join("\n"));

    let op = format!("compact-{}", session::commit_id());
    if let Err(e) = journal.begin_model_op(&op, "compaction").await {
        return ended(&journal, id, None, Err(store_error(e))).await;
    }
    // The summary streams nothing, so only the heartbeat notices a driver
    // that went away: the call is dropped then, and the compaction cancelled.
    let call = turn::summary_call(&system, &history, req.effort, req.trace_parent.clone(), req.focus.as_deref());
    let completion = match select(pin!(call), pin!(events.dropped())).await {
        Either::Left((Ok(c), _)) => c,
        Either::Left((Err(e), _)) => return ended(&journal, id, Some(&op), Err(sample_error(e))).await,
        Either::Right(_) => return ended(&journal, id, Some(&op), Err(sample_error(SampleError::Cancelled))).await,
    };
    counts.steps += 1;
    counts.add(&completion.usage);
    let model = (!completion.model.is_empty()).then(|| completion.model.clone());

    // A refused or filtered summary is never committed: its op closes as
    // failed and the compaction fails, never finishing refusal.
    if let Some(declined) = turn::declined(&completion) {
        let what = if declined == Finish::ContentFilter { "filtered" } else { "refused" };
        let error = RunError::Failed(format!("the summary was {what}"));
        return ended(&journal, id, Some(&op), Err(("failed", error))).await;
    }

    let (summary, replacement) = turn::rebuilt(&history, &completion);
    // Nor is an empty one: it would replace the history with nothing.
    if summary.trim().is_empty() {
        let error = RunError::Failed("the summary was empty".into());
        return ended(&journal, id, Some(&op), Err(("failed", error))).await;
    }
    let kept = replacement.len().saturating_sub(1);
    let mut smaller = history.clone();
    smaller.replace(replacement);
    let after = smaller.estimate_token_count(&system.join("\n"));
    let mut data = json!({
        "trigger": "requested",
        "summary": summary,
        "model": completion.model,
        "kept": kept,
        "tokensBefore": before,
        "tokensAfter": after,
    });
    if let Some(end) = covered {
        data["through"] = json!({ "start": 0, "end": end });
    }
    if let Some(focus) = &req.focus {
        data["focus"] = json!(focus);
    }
    let result = compact_result(id, "done", Some("done"), None, true);
    match journal.compacted(&op, Some(&data), &smaller.raw_items().to_vec(), vec![result]).await {
        Ok(Compacted::Replaced) => {}
        Ok(Compacted::Kept) => {
            let error = RunError::NotRecorded("the smaller history is too large to save".into());
            return ended(&journal, id, None, Err(("not-recorded", error))).await;
        }
        Ok(Compacted::Superseded) => return ended(&journal, id, None, Err(("superseded", RunError::Superseded))).await,
        Err(e) => return ended(&journal, id, None, Err(store_error(e))).await,
    }

    // Recorded: the outcome is done whether or not the driver still reads.
    if let Some(end) = covered {
        let info = CompactionInfo {
            through: SeqRange { start: 0, end },
            trigger: CompactionTrigger::Requested,
            kept: u32::try_from(kept).unwrap_or(u32::MAX),
            tokens_before: u64::try_from(before).ok(),
            tokens_after: u64::try_from(after).ok(),
            seq: journal.last_compaction_seq(),
        };
        if events.put(RunEvent::Compacted(info)).await {
            events.put(RunEvent::Counts(counts_of(&counts))).await;
        }
    }
    Ok(done(&counts, RunFinish::Done, model))
}

fn store_error(e: TurnError) -> (&'static str, RunError) {
    match e {
        TurnError::Superseded => ("superseded", RunError::Superseded),
        e => ("not-recorded", RunError::NotRecorded(format!("saving the session: {e}"))),
    }
}

fn sample_error(e: SampleError) -> (&'static str, RunError) {
    match e {
        SampleError::Cancelled => ("cancelled", RunError::Cancelled),
        SampleError::Open(m) => ("no-model", RunError::NoModel(m)),
        SampleError::Refused(e) => ("model-refused", RunError::ModelRefused(e)),
        SampleError::Failed(e) => ("model-failed", RunError::ModelFailed(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(id: &str) -> RunOptions {
        RunOptions {
            run_id: id.into(),
            instructions: None,
            schema: Some("not even JSON".into()),
            max_steps: None,
            max_total_tokens: None,
            max_duration: None,
            effort: None,
            trace_parent: None,
        }
    }

    #[test]
    fn a_request_is_checked_before_anything_is_written() {
        assert!(validate(None, options("")).is_err());
        assert!(validate(None, options(&"x".repeat(121))).is_err(), "compact:<id> must fit 128 bytes");
        assert!(validate(None, options("a\nb")).is_err());
        assert!(validate(Some("é".repeat(2001)), options("c1")).is_err(), "the focus cap is in characters");
        let ok = validate(Some("é".repeat(2000)), options("c1")).expect("2,000 characters is fine");
        assert_eq!(ok.focus.map(|f| f.chars().count()), Some(2000));
        // The schema does not apply, so it is not checked.
        assert!(validate(None, options("c1")).is_ok());
        assert!(validate(Some("  ".into()), options("c1")).unwrap().focus.is_none(), "a blank focus is none");
        let mut zero = options("c1");
        zero.max_steps = Some(0);
        assert!(validate(None, zero).is_err());
    }

    #[test]
    fn only_model_output_is_worth_summarizing() {
        assert!(!summarizable(&[]));
        let users = vec![ResponseItem::user_text("hi"), ResponseItem::user_text("again")];
        assert!(!summarizable(&users), "the person's messages come back as they went");
        let mut with_reply = users.clone();
        with_reply.push(ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![crate::codex::models::ContentItem::OutputText { text: "hello".into() }],
            opaque: None,
            citations: Vec::new(),
        });
        assert!(summarizable(&with_reply));
    }
}
