//! The session journal over `cosmonic:agent/session@0.4.2`, a host store scoped
//! to THIS workload.
//!
//! There is no session id and no bucket name anywhere in here, and that is the
//! security property: the host binds the session to the calling workload, so
//! this component cannot reach another session and no other workload can reach
//! this one. The host deletes it with the workload.
//!
//! The session holds two things, and every write is one `commit` of both:
//!   * the JOURNAL: what happened, as `agent.step.v1` steps a UI can draw,
//!     `agent.op.v1` before each tool call and `agent.op-result.v1` after it;
//!   * the STATE: Codex's history (`ResponseItem`s), what the model is given next.
//!
//! A turn commits at every point where something happened (see `session.wit`):
//!
//! ```text
//! open_turn:  [unresolved ops closed, step: the person's message] + state
//! record:     [step: the reply, op: each tool call]          (no state)
//!             ... the tool calls ...
//! record:     [op-result: each call, step: the results] + state
//!             ... until the model answers without a tool call ...
//! end_run:    [run-result: how the run ended]
//! ```
//!
//! When the history nears the model's context window, Codex summarizes it
//! (`codex::compact`). That model call is an op like any other, and its result
//! lands with `agent.compaction.v1` and the smaller history as state in one
//! commit:
//!
//! ```text
//! begin_model_op: [op: target model]                          (no state)
//!             ... the summarizing call ...
//! compacted:  [op-result, compaction: the summary] + state
//! ```
//!
//! It only happens with no tool call open, so the state never moves past an
//! unresolved op. The journal keeps every original message.
//!
//! `open_turn`'s commit also carries `agent.run.v1` under the commit id
//! `run:<run-id>`, so the same run id again is answered with the original
//! commit instead of a second run (see `cosmonic:agent/run`).
//!
//! State is never written between an op and its result, so every unresolved op
//! is after the state's `through`, which is what makes a restart safe.

use std::sync::Mutex;

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::bindings::cosmonic::agent::session as store;
use crate::codex::models::ResponseItem;

use store::{CommitOptions, CommitResult, Committed, JournalEnd, NewEntry, Snapshot, StateCondition};

/// A step a UI can represent: `{role, text, blocks, tools}`. Steps written by
/// this agent may also carry `images` (data URLs a tool returned), which a
/// reader that does not know the field skips.
pub const STEP_KIND: &str = "agent.step.v1";
/// The agent is about to call a tool: `{op, target, name, arguments}`.
pub const OP_KIND: &str = "agent.op.v1";
/// How that call ended: `{op, outcome}`.
pub const OP_RESULT_KIND: &str = "agent.op-result.v1";
/// The agent could not read the state it saved and started a fresh context.
pub const CONTEXT_RESET_KIND: &str = "agent.context-reset.v1";
/// A run opened: `{run}`, in the commit that records the person's message.
pub const RUN_KIND: &str = "agent.run.v1";
/// A run ended: `{run, outcome, finish | error}`, before its outcome resolves.
pub const RUN_RESULT_KIND: &str = "agent.run-result.v1";
/// The agent replaced its history with a summary: `{run, trigger (threshold
/// or overflow), summary, model, through, kept, tokensBefore, tokensAfter}`.
/// `through` is `{start, end}`, the inclusive seq range the summary covers.
/// Readers that do not know the kind skip it.
pub const COMPACTION_KIND: &str = "agent.compaction.v1";
/// A requested compaction was accepted: `{compact, focus?}`, in the commit
/// whose commit id is `compact:<compact>` (`cosmonic:agent/compact`).
pub const COMPACT_KIND: &str = "agent.compact.v1";
/// How a requested compaction ended: `{compact, outcome, finish | error,
/// compacted}`. When `compacted` is true it shares a commit with
/// `agent.compaction.v1` and the smaller state.
pub const COMPACT_RESULT_KIND: &str = "agent.compact-result.v1";
/// The agent's whole current plan: `{entries: [{content, status}], run}`,
/// status pending, in-progress or completed. The latest one is the current
/// plan. Committed before the `plan` run event streams.
pub const PLAN_KIND: &str = "agent.plan.v1";

/// How many times a commit whose conditions lost a race is rebuilt and tried
/// again before the turn gives up. Two writers on one session is the case this
/// covers, and it settles in one retry; more means something is looping.
const STALE_RETRIES: usize = 3;
/// How many times a commit the host could not serve (`unavailable`) is resent
/// under the same `commit-id`.
const UNAVAILABLE_RETRIES: usize = 2;
const PAGE: u32 = 1000;

/// Why a turn could not go on.
#[derive(Debug)]
pub enum TurnError {
    /// Another writer kept changing the session while this one tried to start.
    Busy,
    /// A run with this id already started in this session.
    Duplicate,
    /// Another writer took the session over, so this one stopped before making
    /// a call it could no longer record.
    Superseded,
    /// The host refused or failed the call.
    Store(String),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnError::Busy => write!(f, "the session kept changing under this turn; try again"),
            TurnError::Duplicate => write!(f, "a run with this id already started in this session"),
            TurnError::Superseded => write!(
                f,
                "another writer took this session over, so the turn stopped before its next tool call"
            ),
            TurnError::Store(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for TurnError {}

fn describe(e: &store::Error) -> String {
    match e {
        store::Error::TooLarge(t) => match t.entry {
            Some(i) => format!("entry {i} is over the host's {}-byte {:?} limit", t.limit, t.scope),
            None => format!("over the host's {}-byte {:?} limit", t.limit, t.scope),
        },
        store::Error::InvalidArgument(m) => format!("the host refused the call: {m}"),
        store::Error::Unavailable(m) | store::Error::Other(m) => m.clone(),
    }
}

/// How a call this turn made ended. The four outcomes `agent.op-result.v1`
/// defines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
    Unresolved,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Succeeded => "succeeded",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
            Outcome::Unresolved => "unresolved",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpStatus {
    /// The op is in the journal; the call has not been made.
    Committed,
    /// The call is in flight.
    Dispatched,
    Done(Outcome),
}

impl OpStatus {
    /// What to record for an op when it is closed. A call that was never made
    /// was cancelled. One this agent made and then dropped before it returned
    /// (the person stopped the turn) is unresolved: it may or may not have
    /// taken effect.
    fn outcome(self) -> Outcome {
        match self {
            OpStatus::Committed => Outcome::Cancelled,
            OpStatus::Dispatched => Outcome::Unresolved,
            OpStatus::Done(o) => o,
        }
    }
}

struct OpenOp {
    id: String,
    /// The tool's name, for the error step an unresolved call gets.
    name: String,
    status: OpStatus,
}

struct Inner {
    /// The state's version as this writer last wrote or read it; `None` is
    /// `absent`.
    version: Option<String>,
    /// Where the journal ended after this writer's last commit.
    end: JournalEnd,
    /// Ops committed and not yet closed with a result.
    ops: Vec<OpenOp>,
    /// A commit after a call landed without its state (it lost a race); the
    /// state is written at the end of the turn instead.
    state_pending: bool,
    /// Another writer changed the session after this turn started. Conditional
    /// commits from here on are expected to come back stale.
    superseded: bool,
    /// What the session held when a commit last came back stale.
    last_stale: Option<Snapshot>,
    /// The last sequence number before this turn's person's message, read
    /// from the journal once the message landed (never inferred: sequence
    /// numbers may have gaps). `None` when nothing precedes it or it could
    /// not be read.
    before_turn: Option<u64>,
    /// The `seq` of the last `agent.compaction.v1` this writer committed.
    compaction_seq: Option<u64>,
}

/// How a compaction's model op closed, and so what the run does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compacted {
    /// The smaller history is the state: go on with it.
    Replaced,
    /// Nothing was replaced, harmlessly (the summary was refused, or the
    /// smaller state is too large to save): go on with the history the run had.
    Kept,
    /// Another writer owns the session: end the run superseded, with no
    /// further model or tool call.
    Superseded,
}

/// One turn's writer. Created by `open_turn`, which has already committed the
/// person's message.
pub struct Journal {
    inner: Mutex<Inner>,
}

/// A fresh `commit-id`: random, never a counter (counters restart with the
/// agent, and the host keeps the most recent ids, at least 1024; `clear`
/// forgets them). Also a run id for a run that names none (`POST /task`).
pub fn commit_id() -> String {
    use wasip3::random::random::get_random_u64;
    format!("{:016x}{:016x}", get_random_u64(), get_random_u64())
}

/// The first sequence number after `end`.
fn start_after(end: &JournalEnd) -> u64 {
    match end {
        JournalEnd::Empty => 0,
        JournalEnd::At(seq) => seq.saturating_add(1),
    }
}

/// Whether journal end `a` is past `b`.
fn later(a: JournalEnd, b: JournalEnd) -> bool {
    match (a, b) {
        (JournalEnd::At(_), JournalEnd::Empty) => true,
        (JournalEnd::At(x), JournalEnd::At(y)) => x > y,
        (JournalEnd::Empty, _) => false,
    }
}

/// `agent.run-result.v1` for a run that ended: `outcome` is done, failed or
/// cancelled, with `finish` (done) or the run-error case.
pub fn run_result(run: &str, outcome: &str, finish: Option<&str>, error: Option<&str>) -> NewEntry {
    let mut data = json!({ "run": run, "outcome": outcome });
    if let Some(f) = finish {
        data["finish"] = json!(f);
    }
    if let Some(e) = error {
        data["error"] = json!(e);
    }
    entry(RUN_RESULT_KIND, &data)
}

/// `agent.compact-result.v1` for a requested compaction that ended: `outcome`
/// is done, failed or cancelled, with `finish` (done) or the run-error case,
/// and `compacted` says whether it wrote a summary.
pub fn compact_result(id: &str, outcome: &str, finish: Option<&str>, error: Option<&str>, compacted: bool) -> NewEntry {
    let mut data = json!({ "compact": id, "outcome": outcome, "compacted": compacted });
    if let Some(f) = finish {
        data["finish"] = json!(f);
    }
    if let Some(e) = error {
        data["error"] = json!(e);
    }
    entry(COMPACT_RESULT_KIND, &data)
}

fn entry(kind: &str, data: &Value) -> NewEntry {
    NewEntry {
        kind: kind.to_string(),
        data: serde_json::to_vec(data).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

/// One commit, resent under the same `commit-id` while the host is
/// unavailable (so a retry after a lost reply cannot write twice), and with a
/// step's images dropped if they put it over the host's entry limit.
async fn commit(
    entries: Vec<NewEntry>,
    state: Option<Vec<u8>>,
    require_state: Option<StateCondition>,
    require_end: Option<JournalEnd>,
) -> Result<CommitResult, store::Error> {
    commit_as(entries, state, require_state, require_end, None).await
}

/// [`commit`], under a commit id with a meaning (a run's `run:<run-id>`),
/// which every retry keeps.
async fn commit_as(
    mut entries: Vec<NewEntry>,
    state: Option<Vec<u8>>,
    require_state: Option<StateCondition>,
    require_end: Option<JournalEnd>,
    fixed_id: Option<String>,
) -> Result<CommitResult, store::Error> {
    let mut id = fixed_id.clone().unwrap_or_else(commit_id);
    let mut unavailable = 0;
    loop {
        let options = CommitOptions {
            require_state: require_state.clone(),
            require_end,
            commit_id: Some(id.clone()),
        };
        match store::commit(entries.clone(), state.clone(), options).await {
            Err(store::Error::Unavailable(m)) if unavailable < UNAVAILABLE_RETRIES => {
                unavailable += 1;
                eprintln!("session: the host could not take a commit ({m}); resending it");
            }
            Err(store::Error::TooLarge(t)) => {
                // A step too large because of the images it carries is stored
                // without them rather than lost. Anything else is the caller's.
                let without_images = t
                    .entry
                    .and_then(|i| entries.get(i as usize))
                    .filter(|e| e.kind == STEP_KIND)
                    .and_then(|e| step_without_images(&e.data));
                match (t.entry, without_images) {
                    (Some(i), Some(data)) => {
                        entries[i as usize].data = data;
                        // The content changed, so this is a different commit.
                        id = fixed_id.clone().unwrap_or_else(commit_id);
                    }
                    _ => return Err(store::Error::TooLarge(t)),
                }
            }
            other => return other,
        }
    }
}

/// A step without its images, marked so a reader can say they were dropped.
/// `None` when it had none to drop.
pub fn step_without_images(data: &[u8]) -> Option<Vec<u8>> {
    let mut step: Value = serde_json::from_slice(data).ok()?;
    let obj = step.as_object_mut()?;
    if obj.get("images").and_then(Value::as_array).is_none_or(|a| a.is_empty()) {
        return None;
    }
    obj.insert("images".into(), json!([]));
    obj.insert("imagesDropped".into(), json!(true));
    serde_json::to_vec(&step).ok()
}

/// An op in the journal with no result after it.
#[derive(Debug, Clone, PartialEq)]
pub struct Unresolved {
    pub op: String,
    pub target: String,
    pub name: String,
}

/// The ops in `entries` that no `agent.op-result.v1` resolves, oldest first.
/// Entries of other kinds, and data that does not parse, are skipped.
pub fn unresolved_ops<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Vec<Unresolved> {
    let mut open: Vec<Unresolved> = Vec::new();
    for (kind, data) in entries {
        let Ok(v) = serde_json::from_slice::<Value>(data) else { continue };
        let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        match kind {
            OP_KIND => {
                if let Some(op) = field("op") {
                    open.push(Unresolved {
                        op,
                        target: field("target").unwrap_or_default(),
                        name: field("name").unwrap_or_default(),
                    });
                }
            }
            OP_RESULT_KIND => {
                if let Some(op) = field("op") {
                    open.retain(|u| u.op != op);
                }
            }
            _ => {}
        }
    }
    open
}

/// What to commit for ops a previous run left unresolved (`session.wit`'s
/// restart rules).
///
/// A model call may be repeated: it changes nothing outside the session. The
/// state predates it, so the next model call this turn makes IS the repeat;
/// the old op is closed as cancelled.
///
/// A tool call is never repeated here. Repeating one needs the workload's own
/// configuration to say the tool recognizes repeats, and this agent reads no
/// such configuration; a tool's annotations never authorize it. So the op is
/// closed as `unresolved` with an `error` step naming the call, and it is left
/// to a person.
pub fn resolution_entries(unresolved: &[Unresolved]) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    for u in unresolved {
        if u.target == "model" {
            out.push((OP_RESULT_KIND, json!({ "op": u.op, "outcome": Outcome::Cancelled.as_str() })));
            continue;
        }
        out.push((OP_RESULT_KIND, json!({ "op": u.op, "outcome": Outcome::Unresolved.as_str() })));
        out.push((STEP_KIND, unresolved_step(&u.op, &u.name)));
    }
    out
}

/// The `error` step that goes with an `unresolved` tool call, naming it.
fn unresolved_step(op: &str, name: &str) -> Value {
    let shown = if name.is_empty() { "a tool" } else { name };
    json!({
        "role": "error",
        "text": format!(
            "The call to {shown} (call {op}) ended without a result, so it may or may not have \
             happened. It was not repeated; check its effect before asking again."
        ),
        "blocks": ["error"],
        "tools": if name.is_empty() { vec![] } else { vec![name.to_string()] },
    })
}

/// What to commit when this turn closes one of its own ops: the result and,
/// when the call is unresolved, an `error` step naming it.
fn closing_entries(op: &str, name: &str, status: OpStatus) -> Vec<(&'static str, Value)> {
    let outcome = status.outcome();
    let mut out = vec![(OP_RESULT_KIND, json!({ "op": op, "outcome": outcome.as_str() }))];
    if outcome == Outcome::Unresolved {
        out.push((STEP_KIND, unresolved_step(op, name)));
    }
    out
}

/// How a host tool takes its input (`tool-def.input`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A JSON object matching the tool's schema.
    Json,
    /// Raw text, passed to the tool exactly as the model wrote it.
    Freeform,
}

/// The host's tools by name. Calls to any other name never reach the host.
pub type KnownTools = HashMap<String, Input>;

/// An `agent.op.v1` for each function call in `items` that will be
/// dispatched: a known JSON tool with arguments that parse as a JSON object,
/// or a known freeform tool, whose raw input is recorded as a JSON string.
/// Any other call is answered by the agent itself (Codex's "unsupported call"
/// or argument parse error) and never reaches the host, so it gets no op.
pub fn tool_ops(items: &[ResponseItem], known: &KnownTools) -> Vec<(String, Value)> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall { name, arguments, call_id, .. } if known.contains_key(name) => {
                let arguments = match known[name] {
                    Input::Json => serde_json::from_str::<Value>(arguments).ok().filter(Value::is_object)?,
                    Input::Freeform => Value::String(arguments.clone()),
                };
                Some((
                    call_id.clone(),
                    json!({ "op": call_id, "target": "tool", "name": name, "arguments": arguments }),
                ))
            }
            _ => None,
        })
        .collect()
}

/// What the model sees next: Codex's history.
pub type Conversation = Vec<ResponseItem>;

/// The state's format tag. A different tag, or a history this build cannot
/// parse, is unreadable and starts a fresh context.
///
/// A legacy identifier, kept as it is for compatibility: sessions saved by
/// earlier builds carry this exact tag, and changing it would make every one
/// of them unreadable. Do not rename it.
const STATE_FORMAT: &str = "codex-slim/1";

fn encode(history: &Conversation) -> Result<Vec<u8>, TurnError> {
    serde_json::to_vec(&json!({ "format": STATE_FORMAT, "history": history }))
        .map_err(|e| TurnError::Store(e.to_string()))
}

fn decode(value: &[u8]) -> Result<Conversation, String> {
    let v: Value = serde_json::from_slice(value).map_err(|e| e.to_string())?;
    if v.get("format").and_then(Value::as_str) != Some(STATE_FORMAT) {
        return Err("the state is not this agent's format".into());
    }
    serde_json::from_value(v["history"].clone()).map_err(|e| e.to_string())
}

/// The conversation saved in the session, or why it is unusable.
enum Saved {
    None,
    Readable(Conversation),
    /// Written by another agent, or an incompatible version of this one.
    Unreadable,
}

fn saved(snapshot: &Snapshot) -> Saved {
    match snapshot.state.as_ref() {
        None => Saved::None,
        Some(s) => match decode(&s.value) {
            Ok(c) => Saved::Readable(c),
            Err(e) => {
                eprintln!("session: the saved conversation is unreadable by this version ({e})");
                Saved::Unreadable
            }
        },
    }
}

fn condition(version: &Option<String>) -> StateCondition {
    match version {
        Some(v) => StateCondition::Version(v.clone()),
        None => StateCondition::Absent,
    }
}

async fn current() -> Result<Snapshot, TurnError> {
    store::current()
        .await
        .map_err(|e| TurnError::Store(format!("reading the session: {}", describe(&e))))
}

/// Every entry with `seq >= start`, oldest first, as `(seq, kind)`.
async fn read_seqs(mut start: u64) -> Result<Vec<(u64, String)>, TurnError> {
    let mut out = Vec::new();
    loop {
        let page = store::read(start, PAGE)
            .await
            .map_err(|e| TurnError::Store(format!("reading the session: {}", describe(&e))))?;
        let Some(last) = page.last().map(|e| e.seq) else { break };
        out.extend(page.into_iter().map(|e| (e.seq, e.kind)));
        start = last.saturating_add(1);
    }
    Ok(out)
}

/// Where the current turn starts in the journal, from real sequence numbers
/// (they may have gaps, so neither is inferred from the commit's end).
/// `landed` is every entry read from just after `prior_end`, the journal end
/// before the turn's opening commit, as `(seq, kind)`; that commit ended at
/// `commit_end` with the person's step and then `agent.run.v1`, and anything
/// later is another writer's. Returns the last seq before the person's step:
/// an entry the same commit wrote ahead of it (an op a restart closed), else
/// `prior_end`, else `None` for a journal that started with the step.
pub fn before_step(landed: &[(u64, &str)], commit_end: u64, prior_end: Option<u64>) -> Option<u64> {
    let ours = || landed.iter().filter(|(seq, _)| *seq <= commit_end);
    let person = ours().rfind(|(_, kind)| *kind == STEP_KIND).map(|(seq, _)| *seq)?;
    ours().map(|(seq, _)| *seq).rfind(|seq| *seq < person).or(prior_end)
}

/// The last sequence number before the person's step that `open_turn`
/// committed, ending the journal at `end`, read back from the journal.
async fn before_turn(prior: JournalEnd, end: JournalEnd) -> Option<u64> {
    let JournalEnd::At(end) = end else { return None };
    let landed = match read_seqs(start_after(&prior)).await {
        Ok(landed) => landed,
        Err(e) => {
            // The turn is open either way; a summary then records no range.
            eprintln!("session: {e}");
            return None;
        }
    };
    let landed: Vec<(u64, &str)> = landed.iter().map(|(seq, kind)| (*seq, kind.as_str())).collect();
    let prior = match prior {
        JournalEnd::At(seq) => Some(seq),
        JournalEnd::Empty => None,
    };
    before_step(&landed, end, prior)
}

/// Every entry with `seq >= start`, oldest first, as `(seq, kind, data)`.
async fn read_entries(mut start: u64) -> Result<Vec<(u64, String, Vec<u8>)>, TurnError> {
    let mut out = Vec::new();
    loop {
        let page = store::read(start, PAGE)
            .await
            .map_err(|e| TurnError::Store(format!("reading the session: {}", describe(&e))))?;
        let Some(last) = page.last().map(|e| e.seq) else { break };
        out.extend(page.into_iter().map(|e| (e.seq, e.kind, e.data)));
        start = last.saturating_add(1);
    }
    Ok(out)
}

/// Whether `entries` hold an `agent.compact.v1` for compaction `id`.
pub fn compaction_accepted<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>, id: &str) -> bool {
    entries.into_iter().any(|(kind, data)| {
        kind == COMPACT_KIND
            && serde_json::from_slice::<Value>(data).is_ok_and(|v| v.get("compact").and_then(Value::as_str) == Some(id))
    })
}

/// A requested compaction, accepted: its writer, the history it summarizes
/// (`None` when there is none, or none this build can read) and the journal
/// end that history covers.
pub struct OpenedCompaction {
    pub journal: Journal,
    pub conversation: Option<Conversation>,
    pub covered: Option<u64>,
}

/// Accept a requested compaction: refuse an id the journal already has an
/// `agent.compact.v1` for, close any ops a run cut off left unresolved, and
/// commit `agent.compact.v1` under the commit id `compact:<id>`. The host
/// answers a commit id it has seen with the original result rather than
/// writing, and that result's journal end is not past the end read here,
/// which is a retained duplicate too. A lost race is `Busy`. Nothing is
/// written on an error.
pub async fn open_compaction(id: &str, focus: Option<&str>) -> Result<OpenedCompaction, TurnError> {
    let snapshot = current().await?;
    let journal = read_entries(0).await?;
    if compaction_accepted(journal.iter().map(|(_, k, d)| (k.as_str(), d.as_slice())), id) {
        return Err(TurnError::Duplicate);
    }
    let through = snapshot.state.as_ref().map(|s| s.through).unwrap_or(JournalEnd::Empty);
    let after = start_after(&through);
    let unresolved = unresolved_ops(
        journal.iter().filter(|(seq, _, _)| *seq >= after).map(|(_, k, d)| (k.as_str(), d.as_slice())),
    );
    let mut entries: Vec<NewEntry> = resolution_entries(&unresolved).iter().map(|(kind, data)| entry(kind, data)).collect();
    let mut accepted = json!({ "compact": id });
    if let Some(focus) = focus {
        accepted["focus"] = json!(focus);
    }
    entries.push(entry(COMPACT_KIND, &accepted));
    let version = snapshot.state.as_ref().map(|s| s.version.clone());
    let commit_id = Some(format!("compact:{id}"));
    match commit_as(entries, None, Some(condition(&version)), Some(snapshot.end), commit_id).await {
        Ok(CommitResult::Committed(Committed { end, .. })) if !later(end, snapshot.end) => Err(TurnError::Duplicate),
        Ok(CommitResult::Committed(Committed { end, .. })) => Ok(OpenedCompaction {
            journal: Journal {
                inner: Mutex::new(Inner {
                    version,
                    end,
                    ops: Vec::new(),
                    state_pending: false,
                    superseded: false,
                    last_stale: None,
                    before_turn: None,
                    compaction_seq: None,
                }),
            },
            conversation: match saved(&snapshot) {
                Saved::Readable(c) => Some(c),
                Saved::None | Saved::Unreadable => None,
            },
            covered: match through {
                JournalEnd::At(seq) => Some(seq),
                JournalEnd::Empty => None,
            },
        }),
        Ok(CommitResult::Stale(_)) => Err(TurnError::Busy),
        Err(e) => Err(TurnError::Store(format!("accepting the compaction: {}", describe(&e)))),
    }
}

/// Every entry with `seq >= start`, oldest first, as `(kind, data)`.
async fn read_from(mut start: u64) -> Result<Vec<(String, Vec<u8>)>, TurnError> {
    let mut out = Vec::new();
    loop {
        let page = store::read(start, PAGE)
            .await
            .map_err(|e| TurnError::Store(format!("reading the session: {}", describe(&e))))?;
        let Some(last) = page.last().map(|e| e.seq) else { break };
        out.extend(page.into_iter().map(|e| (e.kind, e.data)));
        start = last.saturating_add(1);
    }
    Ok(out)
}

/// Start a turn: load the conversation, close any ops a previous run left
/// unresolved, and commit the person's message with the conversation that now
/// includes it. Returns the writer for the rest of the turn and that
/// conversation.
///
/// A saved conversation this build cannot parse is not an error: the turn
/// starts a fresh context and says so with `agent.context-reset.v1`, and the
/// same commit replaces the unreadable state. Failing every turn instead would
/// strand the session.
///
/// The commit also records `agent.run.v1` for `run`, under the commit id
/// `run:<run>`. The host answers a commit id it has seen with the original
/// result rather than writing, and that result's journal end is not past the
/// end this turn read, which is how a repeated run id (`Duplicate`) is told
/// from a fresh commit. A lost race is `Busy`: the run's commit id is never
/// resent with other content, since the host may retain it with the stale
/// answer. Nothing has happened outside the session yet, so a caller may try
/// again under a new run id.
pub async fn open_turn(user: ResponseItem, user_step: &Value, run: &str) -> Result<(Journal, Conversation), TurnError> {
    let snapshot = current().await?;
    {
        let (prior, reset) = match saved(&snapshot) {
            Saved::None => (Vec::new(), false),
            Saved::Readable(c) => (c, false),
            Saved::Unreadable => (Vec::new(), true),
        };
        let through = snapshot.state.as_ref().map(|s| s.through).unwrap_or(JournalEnd::Empty);
        let after = read_from(start_after(&through)).await?;
        let unresolved = unresolved_ops(after.iter().map(|(k, d)| (k.as_str(), d.as_slice())));

        let mut entries: Vec<NewEntry> = resolution_entries(&unresolved)
            .iter()
            .map(|(kind, data)| entry(kind, data))
            .collect();
        if reset {
            entries.push(entry(CONTEXT_RESET_KIND, &json!({})));
        }
        entries.push(entry(STEP_KIND, user_step));
        entries.push(entry(RUN_KIND, &json!({ "run": run })));

        let mut history = prior;
        history.push(user.clone());
        let conversation = history;
        let state = encode(&conversation)?;

        let version = snapshot.state.as_ref().map(|s| s.version.clone());
        let id = Some(format!("run:{run}"));
        match commit_as(entries, Some(state), Some(condition(&version)), Some(snapshot.end), id).await {
            Ok(CommitResult::Committed(Committed { end, .. })) if !later(end, snapshot.end) => {
                Err(TurnError::Duplicate)
            }
            Ok(CommitResult::Committed(Committed { end, version })) => {
                let before_turn = before_turn(snapshot.end, end).await;
                let journal = Journal {
                    inner: Mutex::new(Inner {
                        version,
                        end,
                        ops: Vec::new(),
                        state_pending: false,
                        superseded: false,
                        last_stale: None,
                        before_turn,
                        compaction_seq: None,
                    }),
                };
                Ok((journal, conversation))
            }
            Ok(CommitResult::Stale(_)) => Err(TurnError::Busy),
            Err(e) => Err(TurnError::Store(format!("saving the message: {}", describe(&e)))),
        }
    }
}

impl Journal {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Single-threaded: a poisoned lock can only mean an earlier panic in
        // this same instance, and the state it guards is still coherent.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Mark a tool call as being made. Refuses a call whose op is not in the
    /// journal, so no call is ever made that a restart could not account for.
    pub fn dispatch(&self, id: &str) -> bool {
        let mut inner = self.lock();
        match inner.ops.iter_mut().find(|o| o.id == id && o.status == OpStatus::Committed) {
            Some(op) => {
                op.status = OpStatus::Dispatched;
                true
            }
            None => false,
        }
    }

    /// Record how a dispatched call ended. Written with the next commit.
    pub fn finish(&self, id: &str, outcome: Outcome) {
        if let Some(op) = self.lock().ops.iter_mut().find(|o| o.id == id) {
            op.status = OpStatus::Done(outcome);
        }
    }

    /// Close every open op, as entries for the next commit (with an error
    /// step for each unresolved call).
    fn take_results(inner: &mut Inner) -> Vec<NewEntry> {
        inner
            .ops
            .drain(..)
            .flat_map(|op| closing_entries(&op.id, &op.name, op.status))
            .map(|(kind, data)| entry(kind, &data))
            .collect()
    }

    /// Commit what one step of the machine applied, before the next step runs
    /// (the next step is where tool calls are made).
    ///
    /// A reply that asks for tools is committed with an op per call and NO
    /// state, conditional on the state and the journal end, so a writer that
    /// has been replaced stops here, before the calls. Anything else closes the
    /// open ops and is committed with the conversation as state.
    pub async fn record(
        &self,
        applied: &[ResponseItem],
        known: &KnownTools,
        steps: Vec<Value>,
        conversation: &Conversation,
    ) -> Result<(), TurnError> {
        let new_ops: Vec<(String, Value)> = tool_ops(applied, known);
        let (mut entries, version, end, superseded) = {
            let mut inner = self.lock();
            let results = Self::take_results(&mut inner);
            (results, inner.version.clone(), inner.end, inner.superseded)
        };
        entries.extend(steps.iter().map(|s| entry(STEP_KIND, s)));
        if entries.is_empty() && new_ops.is_empty() {
            return Ok(());
        }

        if !new_ops.is_empty() {
            entries.extend(new_ops.iter().map(|(_, data)| entry(OP_KIND, data)));
            // Before a call: nothing has happened outside the session, so a
            // lost race means stop rather than call.
            return match commit(entries, None, Some(condition(&version)), Some(end)).await {
                Ok(CommitResult::Committed(c)) => {
                    let mut inner = self.lock();
                    inner.end = c.end;
                    inner.ops = new_ops
                        .into_iter()
                        .map(|(id, data)| OpenOp {
                            id,
                            name: data["name"].as_str().unwrap_or_default().to_string(),
                            status: OpStatus::Committed,
                        })
                        .collect();
                    Ok(())
                }
                Ok(CommitResult::Stale(now)) => {
                    let mut inner = self.lock();
                    inner.superseded = true;
                    inner.last_stale = Some(now);
                    Err(TurnError::Superseded)
                }
                Err(e) => Err(TurnError::Store(format!("recording the tool calls: {}", describe(&e)))),
            };
        }

        self.land(entries, conversation, version, end, superseded).await
    }

    /// Commit entries that record something that already happened, with the
    /// conversation as state when this writer still holds the session.
    ///
    /// After a call the commit must land: on a lost race the entries go again
    /// with no conditions, and the state waits for the end of the turn.
    async fn land(
        &self,
        entries: Vec<NewEntry>,
        conversation: &Conversation,
        version: Option<String>,
        end: JournalEnd,
        superseded: bool,
    ) -> Result<(), TurnError> {
        if !superseded {
            let state = encode(conversation)?;
            match commit(entries.clone(), Some(state), Some(condition(&version)), Some(end)).await {
                Ok(CommitResult::Committed(c)) => {
                    let mut inner = self.lock();
                    inner.end = c.end;
                    inner.version = c.version;
                    inner.state_pending = false;
                    return Ok(());
                }
                Ok(CommitResult::Stale(now)) => {
                    let mut inner = self.lock();
                    inner.superseded = true;
                    inner.last_stale = Some(now);
                }
                // The conversation is over the host's limit: keep the record
                // of what happened, and try the state again at the end.
                Err(store::Error::TooLarge(t)) if t.entry.is_none() => {
                    eprintln!("session: the conversation is too large to save ({} bytes allowed)", t.limit);
                }
                Err(e) => return Err(TurnError::Store(format!("saving the turn: {}", describe(&e)))),
            }
        }
        if entries.is_empty() {
            self.lock().state_pending = true;
            return Ok(());
        }
        match commit(entries, None, None, None).await {
            Ok(CommitResult::Committed(c)) => {
                let mut inner = self.lock();
                inner.end = c.end;
                inner.state_pending = true;
                Ok(())
            }
            // No conditions, so this cannot be stale; treat it as landed.
            Ok(CommitResult::Stale(_)) => Ok(()),
            Err(e) => Err(TurnError::Store(format!("saving the turn: {}", describe(&e)))),
        }
    }

    /// End a turn that did not finish: the machine failed, or the person went
    /// away mid-turn. Closes the open ops (a call never made is cancelled; one
    /// dropped mid-flight is unresolved, with an error step) and stores `steps` (the error, or the text the person was
    /// already shown) with the conversation as far as it got.
    /// `extra` goes last in the same commit: the run's `agent.run-result.v1`.
    pub async fn close(&self, steps: Vec<Value>, conversation: &Conversation, extra: Vec<NewEntry>) {
        let (mut entries, version, end, superseded) = {
            let mut inner = self.lock();
            let results = Self::take_results(&mut inner);
            (results, inner.version.clone(), inner.end, inner.superseded)
        };
        entries.extend(steps.iter().map(|s| entry(STEP_KIND, s)));
        entries.extend(extra);
        if let Err(e) = self.land(entries, conversation, version, end, superseded).await {
            eprintln!("session: could not record the end of the turn: {e}");
        }
    }

    /// The turn's last write, for a turn that finished: when a commit after a
    /// call lost a race, its state was not written, so write it now.
    ///
    /// Another writer changed the session in between. Per the session rules
    /// this takes in what it added: its conversation, then this turn's
    /// messages (`conversation[base..]`, from the person's message on), under
    /// the conditions of the snapshot it returned. If its conversation is in a
    /// format this build cannot read, nothing is overwritten: the turn is in the
    /// journal, and an error step says the conversation was not saved.
    pub async fn settle(&self, conversation: &Conversation, base: usize) {
        let (pending, stale) = {
            let mut inner = self.lock();
            (inner.state_pending, inner.last_stale.take())
        };
        if !pending {
            return;
        }
        let ours = conversation.get(base..).unwrap_or_default().to_vec();
        let mut snapshot = match stale {
            Some(s) => s,
            None => match current().await {
                Ok(s) => s,
                Err(e) => return eprintln!("session: {e}"),
            },
        };
        let mut failure = "another writer kept changing the session".to_string();
        for _ in 0..STALE_RETRIES {
            let mut merged = match saved(&snapshot) {
                Saved::None => Vec::new(),
                Saved::Readable(c) => c,
                Saved::Unreadable => {
                    failure = "another writer saved a conversation this version cannot read".to_string();
                    break;
                }
            };
            merged.extend(ours.iter().cloned());
            let Ok(state) = encode(&merged) else { break };
            let version = snapshot.state.as_ref().map(|s| s.version.clone());
            match commit(Vec::new(), Some(state), Some(condition(&version)), Some(snapshot.end)).await {
                Ok(CommitResult::Committed(c)) => {
                    let mut inner = self.lock();
                    inner.end = c.end;
                    inner.version = c.version;
                    inner.state_pending = false;
                    return;
                }
                Ok(CommitResult::Stale(now)) => snapshot = now,
                Err(e) => {
                    failure = describe(&e);
                    break;
                }
            }
        }
        let step = json!({
            "role": "error",
            "text": format!(
                "This turn is in the history, but the conversation the model sees next could not be \
                 saved: {failure}."
            ),
            "blocks": ["error"],
            "tools": [],
        });
        if let Err(e) = commit(vec![entry(STEP_KIND, &step)], None, None, None).await {
            eprintln!("session: could not record that the conversation was not saved: {}", describe(&e));
        }
    }
}

impl Journal {
    /// The last sequence number before this turn's person's message: the end
    /// of what a summary made before the turn's first model call covers.
    pub fn before_turn(&self) -> Option<u64> {
        self.lock().before_turn
    }

    /// The last sequence number this writer has seen the journal end at: the
    /// end of what a summary made now covers, before its own op.
    pub fn last_seq(&self) -> Option<u64> {
        match self.lock().end {
            JournalEnd::At(seq) => Some(seq),
            JournalEnd::Empty => None,
        }
    }

    /// The `seq` of the `agent.compaction.v1` this writer last committed.
    pub fn last_compaction_seq(&self) -> Option<u64> {
        self.lock().compaction_seq
    }

    /// Commit the op for a model call this agent is about to make, before
    /// making it. Conditional, like a tool call's: a writer that has been
    /// replaced stops here. If the call never returns (the agent restarts),
    /// the next turn closes it as cancelled, since a model call may repeat.
    pub async fn begin_model_op(&self, op: &str, purpose: &str) -> Result<(), TurnError> {
        let (version, end) = {
            let inner = self.lock();
            (inner.version.clone(), inner.end)
        };
        let data = json!({ "op": op, "target": "model", "name": crate::host::MODEL_ALIAS, "purpose": purpose });
        match commit(vec![entry(OP_KIND, &data)], None, Some(condition(&version)), Some(end)).await {
            Ok(CommitResult::Committed(c)) => {
                self.lock().end = c.end;
                Ok(())
            }
            Ok(CommitResult::Stale(now)) => {
                let mut inner = self.lock();
                inner.superseded = true;
                inner.last_stale = Some(now);
                Err(TurnError::Superseded)
            }
            Err(e) => Err(TurnError::Store(format!("recording the model call: {}", describe(&e)))),
        }
    }

    /// Close a compaction's model op. With `compaction` (the entry's data),
    /// the result, the entry and the smaller history as state land in one
    /// conditional commit ([`Compacted::Replaced`]). When the state is over
    /// the host's limit the summary is not used: only the op's result lands,
    /// as failed, with no compaction entry, and the run goes on with the
    /// history it had ([`Compacted::Kept`]). When another writer changed the
    /// session meanwhile, the failed result lands the same way and the run
    /// must stop before its next model or tool call
    /// ([`Compacted::Superseded`]). Without `compaction` (the summary failed
    /// or was refused) the result lands as failed with `conversation`,
    /// unchanged, and a lost race there stops the run too.
    ///
    /// `extra` goes in the installing commit after the entry: a requested
    /// compaction's `agent.compact-result.v1`.
    pub async fn compacted(
        &self,
        op: &str,
        compaction: Option<&Value>,
        conversation: &Conversation,
        extra: Vec<NewEntry>,
    ) -> Result<Compacted, TurnError> {
        let failed = || vec![entry(OP_RESULT_KIND, &json!({ "op": op, "outcome": Outcome::Failed.as_str() }))];
        let (version, end, superseded) = {
            let inner = self.lock();
            (inner.version.clone(), inner.end, inner.superseded)
        };
        let Some(data) = compaction else {
            self.land(failed(), conversation, version, end, superseded).await?;
            return Ok(if self.lock().superseded { Compacted::Superseded } else { Compacted::Kept });
        };
        let outcome = if superseded {
            Compacted::Superseded
        } else {
            let last = extra.is_empty();
            let mut entries = vec![
                entry(OP_RESULT_KIND, &json!({ "op": op, "outcome": Outcome::Succeeded.as_str() })),
                entry(COMPACTION_KIND, data),
            ];
            entries.extend(extra);
            match commit(entries, Some(encode(conversation)?), Some(condition(&version)), Some(end)).await {
                Ok(CommitResult::Committed(c)) => {
                    // The entry's seq: the journal end the commit reports when
                    // the entry is its last (a commit is atomic), else read
                    // back, since seqs may have gaps.
                    let seq = match c.end {
                        JournalEnd::At(seq) if last => Some(seq),
                        JournalEnd::At(seq) => read_seqs(start_after(&end))
                            .await
                            .ok()
                            .and_then(|landed| landed.into_iter().rfind(|(s, k)| *s <= seq && k == COMPACTION_KIND))
                            .map(|(s, _)| s),
                        JournalEnd::Empty => None,
                    };
                    let mut inner = self.lock();
                    inner.end = c.end;
                    inner.version = c.version;
                    inner.state_pending = false;
                    inner.compaction_seq = seq;
                    return Ok(Compacted::Replaced);
                }
                Ok(CommitResult::Stale(now)) => {
                    let mut inner = self.lock();
                    inner.superseded = true;
                    inner.last_stale = Some(now);
                    Compacted::Superseded
                }
                Err(store::Error::TooLarge(t)) => {
                    eprintln!("session: the compaction is too large to save ({} bytes allowed)", t.limit);
                    Compacted::Kept
                }
                Err(e) => return Err(TurnError::Store(format!("saving the compaction: {}", describe(&e)))),
            }
        };
        // The call happened, so its result must land; without conditions, as
        // the fallback of `land` does, but never claiming the compaction.
        match commit(failed(), None, None, None).await {
            Ok(CommitResult::Committed(c)) => self.lock().end = c.end,
            Ok(CommitResult::Stale(_)) => {}
            Err(e) => return Err(TurnError::Store(format!("saving the compaction: {}", describe(&e)))),
        }
        Ok(outcome)
    }

    /// Record the plan the model just set, before the `plan` event streams,
    /// so a reader that reloads sees what a live driver showed. Already true
    /// (the plan is the model's to set, and no op is open for it), so it
    /// lands without conditions, as an entry-only commit.
    pub async fn plan(&self, data: &Value) -> Result<(), TurnError> {
        match commit(vec![entry(PLAN_KIND, data)], None, None, None).await {
            Ok(CommitResult::Committed(c)) => {
                self.lock().end = c.end;
                Ok(())
            }
            Ok(CommitResult::Stale(_)) => Ok(()),
            Err(e) => Err(TurnError::Store(format!("saving the plan: {}", describe(&e)))),
        }
    }

    /// Record how a requested compaction ended, when no summary was
    /// committed with it: the summary call's op closed as failed first, when
    /// there is one, in the same commit. Already true, so it lands without
    /// conditions and leaves the state as it was.
    pub async fn end_compaction(&self, op: Option<&str>, result: NewEntry) {
        let mut entries: Vec<NewEntry> = op
            .map(|op| entry(OP_RESULT_KIND, &json!({ "op": op, "outcome": Outcome::Failed.as_str() })))
            .into_iter()
            .collect();
        entries.push(result);
        match commit(entries, None, None, None).await {
            Ok(CommitResult::Committed(c)) => self.lock().end = c.end,
            Ok(CommitResult::Stale(_)) => {}
            Err(e) => eprintln!("session: could not record the end of the compaction: {}", describe(&e)),
        }
    }

    /// Record that a run ended. Already true, so it lands without conditions.
    pub async fn end_run(&self, result: NewEntry) {
        match commit(vec![result], None, None, None).await {
            Ok(CommitResult::Committed(c)) => self.lock().end = c.end,
            Ok(CommitResult::Stale(_)) => {}
            Err(e) => eprintln!("session: could not record the end of the run: {}", describe(&e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: &str, target: &str, name: &str) -> (String, Vec<u8>) {
        (OP_KIND.into(), serde_json::to_vec(&json!({"op": id, "target": target, "name": name})).unwrap())
    }

    fn result(id: &str) -> (String, Vec<u8>) {
        (OP_RESULT_KIND.into(), serde_json::to_vec(&json!({"op": id, "outcome": "succeeded"})).unwrap())
    }

    #[test]
    fn the_turn_starts_at_the_persons_real_seq_even_across_gaps() {
        let landed = [(17, OP_RESULT_KIND), (20, STEP_KIND), (25, RUN_KIND), (26, STEP_KIND)];
        assert_eq!(before_step(&landed, 25, Some(12)), Some(17), "later writers are not ours");
        assert_eq!(before_step(&[(40, STEP_KIND), (44, RUN_KIND)], 44, Some(31)), Some(31));
        assert_eq!(before_step(&[(0, STEP_KIND), (1, RUN_KIND)], 1, None), None);
        assert_eq!(before_step(&[], 9, Some(3)), None, "nothing readable: no seq is made up");
    }

    #[test]
    fn a_compaction_id_is_known_by_its_accepted_entry() {
        let accepted = serde_json::to_vec(&json!({"compact": "c1", "focus": "the API"})).unwrap();
        let run = serde_json::to_vec(&json!({"run": "c2"})).unwrap();
        let entries = [(COMPACT_KIND, accepted.as_slice()), (RUN_KIND, run.as_slice())];
        assert!(compaction_accepted(entries, "c1"));
        // Run ids are a namespace apart.
        assert!(!compaction_accepted(entries, "c2"));
    }

    #[test]
    fn a_compact_result_says_whether_it_wrote_a_summary() {
        let done = compact_result("c1", "done", Some("done"), None, true);
        assert_eq!(done.kind, COMPACT_RESULT_KIND);
        let v: Value = serde_json::from_slice(&done.data).unwrap();
        assert_eq!(v, json!({"compact": "c1", "outcome": "done", "finish": "done", "compacted": true}));
        let failed: Value =
            serde_json::from_slice(&compact_result("c1", "failed", None, Some("superseded"), false).data).unwrap();
        assert_eq!(failed, json!({"compact": "c1", "outcome": "failed", "error": "superseded", "compacted": false}));
    }

    #[test]
    fn unresolved_ops_are_found_in_order() {
        let entries = [op("m1", "model", "default"), op("t1", "tool", "echo"), result("m1"), op("t2", "tool", "x")];
        let u = unresolved_ops(entries.iter().map(|(k, d)| (k.as_str(), d.as_slice())));
        assert_eq!(u.iter().map(|u| u.op.as_str()).collect::<Vec<_>>(), ["t1", "t2"]);
        let r = resolution_entries(&u);
        assert_eq!(r.iter().filter(|(k, _)| *k == STEP_KIND).count(), 2);
    }

    #[test]
    fn only_dispatchable_calls_get_ops() {
        let call = |name: &str, args: &str, id: &str| ResponseItem::FunctionCall {
            id: None,
            name: name.into(),
            arguments: args.into(),
            call_id: id.into(),
            opaque: None,
            freeform: name == "patch",
        };
        let known: KnownTools = [("echo".to_string(), Input::Json), ("patch".to_string(), Input::Freeform)].into();
        let ops = tool_ops(
            &[
                call("echo", r#"{"text":"hi"}"#, "a"),
                call("echo", "not json", "b"),
                call("nope", "{}", "c"),
                call("patch", "*** Begin Patch\n", "d"),
            ],
            &known,
        );
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].1["arguments"]["text"], "hi");
        assert_eq!(ops[1].1["arguments"], "*** Begin Patch\n");
    }

    #[test]
    fn state_round_trips_and_foreign_state_is_unreadable() {
        let h = vec![ResponseItem::user_text("hi")];
        assert_eq!(decode(&encode(&h).unwrap()).unwrap(), h);
        assert!(decode(br#"{"messages":[]}"#).is_err());
    }

    #[test]
    fn a_dispatched_call_dropped_mid_flight_is_unresolved() {
        let e = closing_entries("t1", "echo", OpStatus::Dispatched);
        assert_eq!(e[0].1["outcome"], "unresolved");
        assert_eq!(e.len(), 2);
        assert_eq!(closing_entries("t1", "echo", OpStatus::Committed)[0].1["outcome"], "cancelled");
    }
}
