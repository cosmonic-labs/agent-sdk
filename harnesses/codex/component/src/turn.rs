//! Codex's turn loop, ported from openai/codex 0.162.0 (c1382380),
//! `codex-rs/core/src/session/turn.rs` (`run_turn`, `run_sampling_request`)
//! and `codex-rs/core/src/stream_events_utils.rs`, Apache-2.0.
//!
//! What upstream does for one turn, and what is kept:
//!
//! 1. Compact before sampling when the history's estimate is at or over the
//!    auto-compact limit (90% of the context window). Kept, with the window
//!    from `models.describe` instead of Codex's model catalog.
//! 2. Loop: build the prompt from `history.for_prompt()` (every call paired
//!    with an output), sample, record the output items, run the function
//!    calls, record their outputs. The model "needs follow-up" when it called
//!    a tool; without one the turn ends with its last message. Kept.
//! 3. After a sample that needs follow-up, compact mid-turn if the token
//!    limit is reached, then continue. Kept.
//! 4. Unknown functions are answered "unsupported call: <name>", arguments
//!    that do not parse with "failed to parse function arguments: ...", and
//!    tool output is truncated to the model's budget (10,000 bytes by
//!    default) before it is recorded. Kept.
//!
//! What is not here: hooks, skills and plugins, MCP server startup, the
//! guardian reviewer, steering input queued during a turn, plan mode, turn
//! diffs, analytics, the time and rollout-budget reminders, parallel tool
//! calls, and every built-in tool but the plan (shell, apply_patch,
//! view_image, web search, MCP). Tools are what the host's `tools` import
//! offers, plus `update_plan`, which the component answers itself
//! (`crate::plan`) and streams as the `plan` run event.
//!
//! Added for `cosmonic:agent/run`: the run's limits (max-steps counts model
//! calls, max-total-tokens, max-duration), the session journal around every
//! tool call, and one retry after compacting when the host refuses a call for
//! length before any reply text was shown (upstream fails the turn
//! instead).


use serde_json::{json, Value};

use crate::bindings::cosmonic::agent::inference_types as llm;
use crate::bindings::cosmonic::agent::run_types::{CompactionInfo, CompactionTrigger, PlanEntry, SeqRange};
use crate::bindings::cosmonic::agent::session::NewEntry;
use crate::bindings::cosmonic::agent::tool_types as tt;
use crate::codex::compact;
use crate::codex::history::ContextManager;
use crate::codex::models::{content_text, ContentItem, ResponseItem};
use crate::codex::{BASE_INSTRUCTIONS, TOOL_OUTPUT_POLICY};
use crate::host::{self, Deltas, SampleError};
use crate::session::{self, Compacted, Conversation, Input, Journal, KnownTools, Outcome, TurnError};

/// Where a run's progress goes. Every method returns false once the reader
/// has gone away, which stops the run.
pub trait Sink: Deltas {
    async fn assistant(&mut self, message: llm::AssistantMessage) -> bool;
    async fn tool_results(&mut self, results: Vec<llm::ToolResult>) -> bool;
    async fn error(&mut self, text: &str) -> bool;
    async fn counts(&mut self, counts: &Counts) -> bool;
    async fn plan(&mut self, plan: Vec<PlanEntry>) -> bool;
    /// Older context was summarized, already journaled as
    /// `agent.compaction.v1`.
    async fn compacted(&mut self, info: CompactionInfo) -> bool;
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Counts {
    pub steps: u32,
    pub tool_calls: u32,
    pub prompt: Option<u64>,
    pub completion: Option<u64>,
}

impl Counts {
    pub(crate) fn add(&mut self, usage: &llm::Usage) {
        fn sum(a: Option<u64>, b: Option<u32>) -> Option<u64> {
            match (a, b) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0).saturating_add(u64::from(b.unwrap_or(0)))),
            }
        }
        self.prompt = sum(self.prompt, usage.prompt_tokens);
        self.completion = sum(self.completion, usage.completion_tokens);
    }

    pub fn total(&self) -> u64 {
        self.prompt.unwrap_or(0).saturating_add(self.completion.unwrap_or(0))
    }
}

pub struct Limits {
    pub max_steps: u32,
    pub max_total_tokens: Option<u64>,
    /// Monotonic-clock instant at which max-duration runs out.
    pub deadline: Option<u64>,
}

pub struct TurnRequest {
    pub run: String,
    pub text: String,
    pub instructions: Option<String>,
    pub schema: Option<String>,
    pub max_steps: u32,
    pub max_total_tokens: Option<u64>,
    pub max_duration: Option<u64>,
    pub effort: Option<llm::Effort>,
    pub trace_parent: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    Done,
    MaxSteps,
    MaxTotalTokens,
    MaxDuration,
    Refusal,
    ContentFilter,
}

impl Finish {
    pub fn as_str(self) -> &'static str {
        match self {
            Finish::Done => "done",
            Finish::MaxSteps => "max-steps",
            Finish::MaxTotalTokens => "max-total-tokens",
            Finish::MaxDuration => "max-duration",
            Finish::Refusal => "refusal",
            Finish::ContentFilter => "content-filter",
        }
    }
}

pub struct Failure {
    /// The run-error case, also written to `agent.run-result.v1`.
    pub case: &'static str,
    pub message: String,
    pub host: Option<SampleError>,
}

pub struct Finished {
    pub reply: Vec<llm::AssistantPart>,
    pub structured: Option<String>,
    pub finish: Finish,
    pub model: Option<String>,
    pub counts: Counts,
}

pub enum Ended {
    Done(Finished),
    Failed(Failure),
    Cancelled,
}

/// How the loop stopped before a reply.
enum Stop {
    Cancelled,
    Failed(Failure),
}

impl From<TurnError> for Stop {
    fn from(e: TurnError) -> Self {
        match e {
            TurnError::Superseded => Stop::Failed(Failure { case: "superseded", message: e.to_string(), host: None }),
            e => Stop::Failed(Failure { case: "not-recorded", message: format!("saving the session: {e}"), host: None }),
        }
    }
}

fn sample_failure(e: SampleError) -> Stop {
    let case = match &e {
        SampleError::Cancelled => return Stop::Cancelled,
        SampleError::Open(_) => "no-model",
        SampleError::Refused(_) => "model-refused",
        SampleError::Failed(_) => "model-failed",
    };
    Stop::Failed(Failure { case, message: e.to_string(), host: Some(e) })
}

fn now() -> u64 {
    wasip3::clocks::monotonic_clock::now()
}

/// A run the journal has opened.
pub struct Accepted {
    pub journal: Journal,
    pub conversation: Conversation,
    /// Where this run's items start: the person's message.
    pub base: usize,
    pub deadline: Option<u64>,
}

// ----- journal steps (`agent.step.v1`) -----

fn assistant_step(items: &[ResponseItem], run: &str) -> Value {
    let mut text = String::new();
    let mut blocks: Vec<&str> = Vec::new();
    let mut tools: Vec<String> = Vec::new();
    for item in items {
        let block = match item {
            ResponseItem::Message { content, .. } => {
                let refusal: String = content
                    .iter()
                    .filter_map(|c| match c {
                        ContentItem::Refusal { refusal } => Some(refusal.as_str()),
                        _ => None,
                    })
                    .collect();
                if refusal.is_empty() {
                    text.push_str(&content_text(content));
                    "text"
                } else {
                    text.push_str(&refusal);
                    "refusal"
                }
            }
            ResponseItem::Reasoning { .. } => "thinking",
            ResponseItem::FunctionCall { name, .. } => {
                tools.push(name.clone());
                "toolRequest"
            }
            ResponseItem::HostBlock { .. } => "other",
            _ => continue,
        };
        if !blocks.contains(&block) {
            blocks.push(block);
        }
    }
    json!({ "role": "assistant", "text": text, "blocks": blocks, "tools": tools, "run": run })
}

fn tool_step(outputs: &[(String, ResponseItem)], run: &str) -> Value {
    let text = outputs
        .iter()
        .filter_map(|(_, item)| match item {
            ResponseItem::FunctionCallOutput { output, .. } => Some(output.text()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tools: Vec<&str> = outputs.iter().map(|(name, _)| name.as_str()).collect();
    json!({ "role": "tool", "text": text, "blocks": ["toolResponse"], "tools": tools, "run": run })
}

pub fn error_step(text: &str, run: &str) -> Value {
    json!({ "role": "error", "text": text, "blocks": ["error"], "tools": [], "run": run })
}

/// A model reply as a driver draws it: no continuation data, no provider blocks.
fn for_driver(message: &llm::AssistantMessage) -> llm::AssistantMessage {
    llm::AssistantMessage {
        content: without_opaque(message)
            .into_iter()
            .filter(|p| !matches!(p.content, llm::AssistantContent::Other(_)))
            .collect(),
    }
}

fn without_opaque(message: &llm::AssistantMessage) -> Vec<llm::AssistantPart> {
    message
        .content
        .iter()
        .map(|p| llm::AssistantPart { content: p.content.clone(), opaque: None, citations: p.citations.clone() })
        .collect()
}

/// A reply the model declined or a filter withheld: the run's finish when it
/// is the last assistant step (`run-types.finish`, precedence).
pub(crate) fn declined(completion: &llm::Completion) -> Option<Finish> {
    match completion.finish_reason {
        llm::FinishReason::Refusal => return Some(Finish::Refusal),
        llm::FinishReason::ContentFilter => return Some(Finish::ContentFilter),
        _ => {}
    }
    completion
        .message
        .content
        .iter()
        .any(|p| matches!(p.content, llm::AssistantContent::Refusal(_)))
        .then_some(Finish::Refusal)
}

/// Why the agent compacted, as the `trigger` of `agent.compaction.v1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The history passed the auto-compact limit.
    Threshold,
    /// The host refused a call for length, and the agent compacted to retry.
    Overflow,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Threshold => "threshold",
            Trigger::Overflow => "overflow",
        }
    }

    fn wit(self) -> CompactionTrigger {
        match self {
            Trigger::Threshold => CompactionTrigger::Threshold,
            Trigger::Overflow => CompactionTrigger::Overflow,
        }
    }
}

/// Codex's summary call over `history`: the history, then the summarization
/// prompt as a user message, then (for a requested compaction with a focus)
/// the person's own words about what to keep, as their own user message,
/// never as instructions. No tools. On a refusal for length the oldest item
/// is dropped and the call made again while more than one is left, as
/// upstream does; the attempts the host refused never ran.
pub(crate) async fn summary_call(
    system: &[String],
    history: &ContextManager,
    effort: Option<llm::Effort>,
    trace_parent: Option<String>,
    focus: Option<&str>,
) -> Result<llm::Completion, SampleError> {
    let mut source = history.clone();
    loop {
        let mut items = source.for_prompt();
        match focus {
            Some(focus) => {
                items.push(ResponseItem::user_text(format!("{}\n\n{}", compact::SUMMARIZATION_PROMPT, compact::FOCUS_NOTE)));
                items.push(ResponseItem::user_text(focus));
            }
            None => items.push(ResponseItem::user_text(compact::SUMMARIZATION_PROMPT)),
        }
        let messages = host::to_messages(system, &items);
        let options = host::chat_options(&[], None, effort, trace_parent.clone());
        match host::sample(messages, options, &mut Muted).await {
            Ok(s) => return Ok(s.completion),
            Err(SampleError::Refused(llm::RequestError::ContextLengthExceeded(_))) if source.len() > 1 => {
                source.remove_first_item();
            }
            Err(e) => return Err(e),
        }
    }
}

/// The summary a completion holds, and Codex's rebuilt history: the person's
/// recent messages, then the summary under its prefix. A reply may arrive as
/// several text parts, each its own item here, so all of them make the
/// summary (upstream's Responses API gives one message).
pub(crate) fn rebuilt(history: &ContextManager, completion: &llm::Completion) -> (String, Vec<ResponseItem>) {
    let reply = host::to_items(&completion.message);
    let summary = compact::assistant_text_since_last_user(&reply);
    let summary_text = format!("{}\n{}", compact::SUMMARY_PREFIX, summary);
    let users = compact::collect_user_messages(history.raw_items());
    (summary, compact::build_compacted_history(&users, &summary_text))
}

/// Compaction's own model call streams nothing to the reader.
struct Muted;

impl Deltas for Muted {
    async fn text(&mut self, _: &str) -> bool {
        true
    }
    async fn reasoning(&mut self, _: &str) -> bool {
        true
    }
}

struct Turn<'a, S: Sink> {
    sink: &'a mut S,
    journal: &'a Journal,
    run: &'a str,
    history: ContextManager,
    system: Vec<String>,
    tools: Vec<tt::ToolDef>,
    known: KnownTools,
    schema: Option<String>,
    effort: Option<llm::Effort>,
    trace_parent: Option<String>,
    limits: Limits,
    /// Upstream's `auto_compact_token_limit`: 90% of the context window.
    auto_compact_limit: Option<i64>,
    counts: Counts,
    model: Option<String>,
    overflow_retry: bool,
    /// The reply parts of the run's last assistant step.
    last_reply: Vec<llm::AssistantPart>,
    /// Set when compaction replaced the history, so this run's items no
    /// longer start at `base`.
    compacted: bool,
}

impl<S: Sink> Turn<'_, S> {
    fn estimate(&self) -> i64 {
        let system: String = self.system.join("\n");
        self.history.estimate_token_count(&system)
    }

    fn over_limit(&self) -> bool {
        self.auto_compact_limit.is_some_and(|limit| self.estimate() >= limit)
    }

    fn past_deadline(&self) -> bool {
        self.limits.deadline.is_some_and(|d| now() >= d)
    }

    async fn counted(&mut self, usage: &llm::Usage, model: &str) -> Result<(), Stop> {
        self.counts.steps += 1;
        self.counts.add(usage);
        if !model.is_empty() {
            self.model = Some(model.to_string());
        }
        if self.sink.counts(&self.counts).await {
            Ok(())
        } else {
            Err(Stop::Cancelled)
        }
    }

    /// Codex's inline auto-compaction (`run_inline_auto_compact_task`).
    /// `held` goes after the summary, outside what is summarized. The summary
    /// call is an op in the journal before it is made, and its result lands
    /// with `agent.compaction.v1` and the smaller history in one commit.
    /// Returns whether the history was replaced: a refused or filtered
    /// summary, or a smaller history too large to save, leaves it as it was
    /// (the op closed as failed) and the run goes on with it.
    async fn compact(&mut self, trigger: Trigger, held: Option<ResponseItem>) -> Result<bool, Stop> {
        let before = self.estimate();
        // What the summary covers in the journal: before the turn's first
        // call, everything ahead of the person's message (held back here);
        // later, everything so far.
        let through = if held.is_some() { self.journal.before_turn() } else { self.journal.last_seq() };
        // The history as it stands, held message included: the state a
        // summary that is not used leaves.
        let unchanged: Conversation = self.history.raw_items().iter().cloned().chain(held.clone()).collect();
        let op = format!("compact-{}", session::commit_id());
        self.journal.begin_model_op(&op, "compaction").await?;
        let completion = match summary_call(&self.system, &self.history, self.effort, self.trace_parent.clone(), None).await {
            Ok(c) => c,
            Err(e) => {
                // The call failed: its op closes as failed, then the run
                // fails as upstream's does.
                self.journal.compacted(&op, None, &unchanged, Vec::new()).await?;
                return Err(sample_failure(e));
            }
        };
        self.counted(&completion.usage, &completion.model).await?;
        // A refused, filtered or empty summary is never committed: the run
        // goes on with the context it had.
        let (summary, mut replacement) = rebuilt(&self.history, &completion);
        if declined(&completion).is_some() || summary.trim().is_empty() {
            return match self.journal.compacted(&op, None, &unchanged, Vec::new()).await? {
                Compacted::Superseded => Err(TurnError::Superseded.into()),
                _ => Ok(false),
            };
        }
        // Carried over verbatim: the person's recent messages and the held one.
        let kept = replacement.len().saturating_sub(1) + usize::from(held.is_some());
        replacement.extend(held);
        let mut smaller = self.history.clone();
        smaller.replace(replacement);
        let after = smaller.estimate_token_count(&self.system.join("\n"));
        let mut data = json!({
            "run": self.run,
            "trigger": trigger.as_str(),
            "summary": summary,
            "model": completion.model,
            "kept": kept,
            "tokensBefore": before,
            "tokensAfter": after,
        });
        if let Some(end) = through {
            data["through"] = json!({ "start": 0, "end": end });
        }
        match self.journal.compacted(&op, Some(&data), &smaller.raw_items().to_vec(), Vec::new()).await? {
            Compacted::Replaced => {}
            Compacted::Kept => return Ok(false),
            Compacted::Superseded => return Err(TurnError::Superseded.into()),
        }
        self.history = smaller;
        self.compacted = true;
        // Committed, so the driver may mark the point now; the summary itself
        // is read from the journal. With no range (the turn's start could not
        // be read back) there is nothing to mark it by.
        match through {
            Some(end) => {
                let info = CompactionInfo {
                    through: SeqRange { start: 0, end },
                    trigger: trigger.wit(),
                    kept: u32::try_from(kept).unwrap_or(u32::MAX),
                    tokens_before: u64::try_from(before).ok(),
                    tokens_after: u64::try_from(after).ok(),
                    seq: self.journal.last_compaction_seq(),
                };
                if !self.sink.compacted(info).await {
                    return Err(Stop::Cancelled);
                }
            }
            None => eprintln!("compaction ({}): no journal range to report", trigger.as_str()),
        }
        Ok(true)
    }

    /// One model call and what it produced, with the overflow retry.
    async fn sample(&mut self) -> Result<host::Sampled, Stop> {
        loop {
            let prompt = self.history.for_prompt();
            let messages = host::to_messages(&self.system, &prompt);
            let options = host::chat_options(&self.tools, self.schema.as_deref(), self.effort, self.trace_parent.clone());
            let overflowed = match host::sample(messages, options, &mut *self.sink).await {
                Ok(s) => return Ok(s),
                Err(SampleError::Refused(llm::RequestError::ContextLengthExceeded(_))) => true,
                Err(e) => return Err(sample_failure(e)),
            };
            // Only reachable before any reply text: a refused request streams
            // nothing. A generation that failed for length may already have
            // streamed text, so it is not retried.
            if overflowed && self.overflow_retry {
                self.overflow_retry = false;
                // Nothing replaced: the same call would overflow again.
                if self.compact(Trigger::Overflow, None).await? {
                    continue;
                }
            }
            return Err(Stop::Failed(Failure {
                case: "model-refused",
                message: "the conversation does not fit the model's context window".into(),
                host: None,
            }));
        }
    }

    /// Run the function calls of one reply, in order. Upstream runs calls to
    /// tools that support it in parallel; host tools carry no such flag, so
    /// each runs after the previous one returns.
    async fn run_tools(&mut self, items: &[ResponseItem]) -> Result<Vec<(String, ResponseItem)>, Stop> {
        let mut outputs = Vec::new();
        for item in items {
            let ResponseItem::FunctionCall { name, arguments, call_id, freeform, .. } = item else { continue };
            self.counts.tool_calls += 1;
            let output = if name == crate::plan::NAME {
                match crate::plan::parse(arguments) {
                    Ok(plan) => {
                        self.journal.plan(&crate::plan::entry(&plan, self.run)).await?;
                        if !self.sink.plan(plan).await {
                            return Err(Stop::Cancelled);
                        }
                        crate::codex::models::FunctionCallOutputPayload::from_text(crate::plan::OUTPUT.into())
                    }
                    Err(e) => crate::codex::models::FunctionCallOutputPayload::from_text(e),
                }
            } else if !self.known.contains_key(name) {
                crate::codex::models::FunctionCallOutputPayload {
                    body: crate::codex::models::FunctionCallOutputBody::Text(format!("unsupported call: {name}")),
                    success: Some(false),
                }
            // A freeform tool's input is raw text, never parsed here; the
            // tool says when it cannot take it.
            } else if let Some(e) = (self.known[name] == Input::Json).then(|| not_an_object(arguments)).flatten() {
                crate::codex::models::FunctionCallOutputPayload::from_text(format!(
                    "failed to parse function arguments: {e}"
                ))
            } else if self.past_deadline() {
                // Never dispatched: the journal closes the op as cancelled.
                break;
            } else {
                if !self.journal.dispatch(call_id) {
                    return Err(Stop::Failed(Failure {
                        case: "failed",
                        message: format!("tool call {call_id} has no op in the journal"),
                        host: None,
                    }));
                }
                // The model's arguments as it wrote them, json or text.
                let request = llm::ToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: host::tool_arguments(arguments, *freeform),
                };
                match host::call_tool(request).await {
                    Ok(response) => {
                        self.journal.finish(call_id, if response.is_error { Outcome::Failed } else { Outcome::Succeeded });
                        host::output_of(response)
                    }
                    Err(error) => {
                        let outcome = match &error {
                            tt::CallError::NotDispatched(_) => Outcome::Cancelled,
                            tt::CallError::OutcomeUnknown(_) => Outcome::Unresolved,
                        };
                        self.journal.finish(call_id, outcome);
                        host::output_of_error(name, &error)
                    }
                }
            };
            outputs.push((
                name.clone(),
                ResponseItem::FunctionCallOutput { id: None, call_id: Some(call_id.clone()), output },
            ));
        }
        Ok(outputs)
    }

    async fn turn(&mut self) -> Result<Finish, Stop> {
        // `run_pre_sampling_compact`. Upstream compacts before it records the
        // turn's input, so the person's new message follows the summary. Here
        // the journal has already committed it (the run is accepted only
        // once it has), so it is held back from the compaction and put after
        // the summary.
        if self.over_limit() {
            let mut items = self.history.raw_items().to_vec();
            let held = items.pop();
            self.history.replace(items);
            let replaced = self.compact(Trigger::Threshold, held.clone()).await;
            if !matches!(replaced, Ok(true)) {
                // Nothing was replaced; put the message back.
                let mut items = self.history.raw_items().to_vec();
                items.extend(held);
                self.history.replace(items);
            }
            replaced?;
        }
        loop {
            if self.counts.steps >= self.limits.max_steps {
                return Ok(Finish::MaxSteps);
            }
            if self.past_deadline() {
                return Ok(Finish::MaxDuration);
            }
            let sampled = self.sample().await?;
            let completion = sampled.completion;
            let items = host::to_items(&completion.message);
            self.history.record_items(items.iter(), TOOL_OUTPUT_POLICY);
            let step = assistant_step(&items, self.run);
            self.journal.record(&items, &self.known, vec![step], &self.history.raw_items().to_vec()).await?;
            let shown = for_driver(&completion.message);
            self.last_reply = without_opaque(&completion.message);
            if !shown.content.is_empty() && !self.sink.assistant(shown).await {
                return Err(Stop::Cancelled);
            }
            self.counted(&completion.usage, &completion.model).await?;

            // Upstream fails a response that ended incomplete.
            match &completion.finish_reason {
                llm::FinishReason::MaxTokens | llm::FinishReason::ContextLength => {
                    return Err(Stop::Failed(Failure {
                        case: "model-failed",
                        message: format!("the model's reply was cut off ({:?})", completion.finish_reason),
                        host: None,
                    }));
                }
                _ => {}
            }
            // A declined reply ends the run, tool calls and all.
            if let Some(finish) = declined(&completion) {
                return Ok(finish);
            }

            let needs_follow_up = items.iter().any(|i| matches!(i, ResponseItem::FunctionCall { .. }));
            if !needs_follow_up {
                // The provider paused a long turn: what it sent is recorded,
                // and the next call lets the model continue.
                if matches!(completion.finish_reason, llm::FinishReason::PauseTurn) {
                    continue;
                }
                return Ok(Finish::Done);
            }
            if self.limits.max_total_tokens.is_some_and(|m| self.counts.total() >= m) {
                return Ok(Finish::MaxTotalTokens);
            }

            let outputs = self.run_tools(&items).await?;
            let recorded: Vec<ResponseItem> = outputs.iter().map(|(_, i)| i.clone()).collect();
            self.history.record_items(recorded.iter(), TOOL_OUTPUT_POLICY);
            let step = tool_step(&outputs, self.run);
            self.journal.record(&[], &self.known, vec![step], &self.history.raw_items().to_vec()).await?;
            let results = match host::to_messages(&[], &recorded).pop() {
                Some(llm::Message::Tool(results)) => results,
                _ => Vec::new(),
            };
            if !results.is_empty() && !self.sink.tool_results(results).await {
                return Err(Stop::Cancelled);
            }
            if self.past_deadline() {
                return Ok(Finish::MaxDuration);
            }
            // Upstream: `token_limit_reached && needs_follow_up` compacts mid-turn.
            if self.over_limit() {
                self.compact(Trigger::Threshold, None).await?;
            }
        }
    }
}

/// Why JSON tool arguments are unusable, if they are.
fn not_an_object(arguments: &str) -> Option<String> {
    match serde_json::from_str::<Value>(arguments) {
        Ok(v) if v.is_object() => None,
        Ok(_) => Some("arguments must be a JSON object".into()),
        Err(e) => Some(e.to_string()),
    }
}

/// The reply as JSON, with a fenced code block around it taken off. Whether
/// it satisfies the schema is the driver's to check.
fn structured(answer: &str) -> Result<String, String> {
    let trimmed = answer.trim();
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    serde_json::from_str::<Value>(inner)
        .map(|_| inner.to_string())
        .map_err(|e| format!("the reply did not match the schema: not JSON ({e})"))
}

/// A note for sessions whose tools can only read, so the model says it
/// cannot make a change instead of writing as if it had. Small models ignore
/// the general rule in the base prompt; a statement about this session's
/// tools lands. The hints are the tools' own claims: a tool that wrongly
/// claims to only read can only make the agent decline a change it could
/// have made, never reach further.
fn read_only_note(tools: &[tt::ToolDef]) -> Option<String> {
    if tools.iter().any(|t| t.annotations.read_only != Some(true)) {
        return None;
    }
    Some(
        "In this session every tool you have only reads. You cannot edit, create or delete \
         files, run commands or tests, or commit. When asked to do any of these, say in your \
         first sentence that you cannot, then do what you can: read the code and show the \
         exact change as a unified diff for the person to apply."
            .to_string(),
    )
}

/// Run one turn, streaming its progress to `sink`, and record how it ended.
/// The run's last commit comes before this returns.
pub async fn work(sink: &mut impl Sink, req: &TurnRequest, acc: Accepted) -> Ended {
    let Accepted { journal, conversation, base, deadline } = acc;
    let run = req.run.as_str();

    let (mut tools, list_error) = match host::list_tools().await {
        Ok(tools) => (tools, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    // The plan tool is the agent's own and always offered. A host tool by
    // the same name could not be told apart from it, so it is not offered.
    if tools.iter().any(|t| t.name == crate::plan::NAME) {
        eprintln!("codex-agent: the host offers a tool named {}; the agent's own plan tool is used instead", crate::plan::NAME);
        tools.retain(|t| t.name != crate::plan::NAME);
    }
    let known: KnownTools = tools
        .iter()
        .map(|t| {
            let input = match t.input {
                llm::ToolInput::JsonSchema(_) => Input::Json,
                llm::ToolInput::Grammar(_) | llm::ToolInput::Text => Input::Freeform,
            };
            (t.name.clone(), input)
        })
        .collect();
    let read_only = read_only_note(&tools);
    tools.push(crate::plan::tool_def());
    let mut system = Vec::new();
    if let Some(i) = &req.instructions {
        system.push(i.clone());
    }
    system.push(BASE_INSTRUCTIONS.to_string());
    system.extend(read_only);
    if let Some(s) = &req.schema {
        system.push(format!(
            "Your final answer must be only a JSON value that satisfies this JSON Schema, with no other text: {s}"
        ));
    }
    let auto_compact_limit = host::context_length().await.map(|w| i64::from(w) * 9 / 10);
    let mut turn = Turn {
        known,
        tools,
        sink,
        journal: &journal,
        run,
        history: ContextManager::new(conversation),
        system,
        schema: req.schema.clone(),
        effort: req.effort,
        trace_parent: req.trace_parent.clone(),
        limits: Limits { max_steps: req.max_steps, max_total_tokens: req.max_total_tokens, deadline },
        auto_compact_limit,
        counts: Counts::default(),
        model: None,
        overflow_retry: true,
        last_reply: Vec::new(),
        compacted: false,
    };

    if let Some(e) = list_error {
        // A run may continue after an error step; this one continues with no tools.
        let text = format!("Listing tools failed, so this run has none: {e}");
        // A failure here surfaces again at the loop's first commit or write.
        let history = turn.history.raw_items().to_vec();
        let _ = turn.journal.record(&[], &turn.known, vec![error_step(&text, run)], &history).await;
        let _ = turn.sink.error(&text).await;
    }

    let result = turn.turn().await;
    let history: Conversation = turn.history.raw_items().to_vec();
    let counts = turn.counts;
    let base = if turn.compacted { 0 } else { base };

    let close = |steps: Vec<Value>, extra: Vec<NewEntry>| {
        let history = history.clone();
        let journal = &journal;
        async move { journal.close(steps, &history, extra).await }
    };

    match result {
        Err(Stop::Cancelled) => {
            let steps = vec![error_step("the run was cancelled", run)];
            close(steps, vec![session::run_result(run, "cancelled", None, Some("cancelled"))]).await;
            Ended::Cancelled
        }
        Err(Stop::Failed(failure)) => {
            let _ = turn.sink.error(&failure.message).await;
            let steps = vec![error_step(&failure.message, run)];
            close(steps, vec![session::run_result(run, "failed", None, Some(failure.case))]).await;
            Ended::Failed(failure)
        }
        Ok(finish) => {
            let reply = turn.last_reply.clone();
            let answer = reply
                .iter()
                .filter_map(|p| match &p.content {
                    llm::AssistantContent::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<String>();
            let structured = match &req.schema {
                Some(_) if finish == Finish::Done => match structured(&answer) {
                    Ok(s) => Some(s),
                    Err(message) => {
                        let failure = Failure { case: "output-rejected", message, host: None };
                        let steps = vec![error_step(&failure.message, run)];
                        close(steps, vec![session::run_result(run, "failed", None, Some(failure.case))]).await;
                        return Ended::Failed(failure);
                    }
                },
                _ => None,
            };
            journal.settle(&history, base).await;
            journal.end_run(session::run_result(run, "done", Some(finish.as_str()), None)).await;
            Ended::Done(Finished { reply, structured, finish, model: turn.model.clone(), counts })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_tools_get_the_session_note() {
        assert!(read_only_note(&[crate::plan::tool_def()]).is_some());
        assert!(read_only_note(&[]).is_some());
        let mut writer = crate::plan::tool_def();
        writer.annotations.read_only = None;
        assert!(read_only_note(&[crate::plan::tool_def(), writer.clone()]).is_none());
        writer.annotations.read_only = Some(false);
        assert!(read_only_note(&[writer]).is_none());
    }
}
