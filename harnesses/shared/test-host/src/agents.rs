//! The contract every harness's typed `run` and `compact` exports meet,
//! checked the same way for each: a plain run, a duplicate run id, a run
//! cancelled by dropping its events, and the `compact` export (nothing to
//! summarize, a summary with the person's focus that the next run reads, a
//! repeated id, one asked for during a run, a refused summary, a spent limit,
//! one dropped mid-summary). Then the checks that only hold for the agents
//! that keep the person's latest exchange verbatim.

use std::future::Future;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::json;

use crate::fixture::*;
use crate::host::run_types::{CompactionTrigger, Finish, RunError, RunEvent, StartError};
use crate::host::types::Message;
use crate::host::{self, Entry};

/// Each case's outcome, printed as it ends, and the count of each.
#[derive(Default)]
pub(crate) struct Tally {
    pub(crate) passed: usize,
    pub(crate) failed: usize,
}

impl Tally {
    pub(crate) async fn case(&mut self, name: &str, check: impl Future<Output = Result<()>>) {
        match check.await {
            Ok(()) => {
                self.passed += 1;
                println!("ok   {name}");
            }
            Err(e) => {
                self.failed += 1;
                println!("FAIL {name}: {e:#}");
            }
        }
    }
}

/// What a requested summary covers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Covers {
    /// Everything before the person's latest message, which is kept verbatim
    /// with what followed it (Pi).
    BeforeLatestExchange,
    /// The whole journal so far (Codex).
    All,
}

/// The shared contract, in order on one fresh session.
pub(crate) async fn contract_suite(t: &mut Tally, f: &Fixture, covers: Covers) {
    t.case("a compaction on an empty session has nothing to summarize: done, silent", requested_nothing(f)).await;
    t.case("a plain run", plain_run(f)).await;
    t.case("a repeated run id is a duplicate", duplicate(f)).await;
    t.case("dropping the events cancels the run, recorded", cancelled(f)).await;
    t.case("three more exchanges", more_runs(f)).await;
    t.case("a requested compaction summarizes with the person's focus", requested(f, covers)).await;
    t.case("the next run reads the summary, not what it replaced", after_requested(f, covers)).await;
    t.case("the same compaction id again is a duplicate", requested_again(f)).await;
    t.case("a compaction during a run is busy", requested_busy(f)).await;
    t.case("a refused summary is never committed", requested_refused(f)).await;
    t.case("a spent limit stops a compaction before its call", requested_limit(f)).await;
    t.case("dropping a compaction mid-summary cancels it at once", compaction_dropped_mid_summary(f)).await;
}

/// Long enough that a summary is smaller than what it replaces.
fn padded(topic: &str) -> String {
    format!("Tell me about {topic}. {}", "Some context the summary can drop. ".repeat(40))
}

/// The seq of the last entry before the person's latest step at or before
/// `covered`: the end of what a summary that keeps that message covers.
fn before_last_person(entries: &[Entry], covered: u64) -> Option<u64> {
    let upto: Vec<&Entry> = entries.iter().filter(|e| e.seq <= covered).collect();
    let person = upto.iter().rfind(|e| e.kind == "agent.step.v1" && e.json()["role"] == "user")?.seq;
    upto.iter().map(|e| e.seq).rfind(|s| *s < person)
}

fn state_bytes(f: &Fixture) -> usize {
    f.agent().state.as_ref().map_or(0, |s| s.value.len())
}

/// A compaction on an empty session has nothing to summarize: done, no
/// event, no model call, agent.compact-result.v1 alone after its acceptance,
/// and no state written.
async fn requested_nothing(f: &Fixture) -> Result<()> {
    let (events, outcome) = started(f.compact("c0", None, |_| {}).await?)?;
    ensure!(events.is_empty(), "events: {:?}", events.iter().map(describe).collect::<Vec<_>>());
    ensure!(matches!(outcome.finish, Finish::Done) && outcome.reply.is_empty() && outcome.structured.is_none());
    ensure!(outcome.counts.steps == 0, "{:?}", outcome.counts);
    let agent = f.agent();
    ensure!(agent.chats.is_empty(), "a model call was made");
    ensure!(agent.kinds(0) == ["agent.compact.v1", "agent.compact-result.v1"], "journal: {:?}", agent.kinds(0));
    let result = agent.entries[1].json();
    ensure!(result == json!({"compact": "c0", "outcome": "done", "finish": "done", "compacted": false}), "{result}");
    ensure!(agent.state.is_none(), "the state was written");
    Ok(())
}

/// A plain run: the reply, and the journal around it.
async fn plain_run(f: &Fixture) -> Result<()> {
    let from = f.agent().entries.len();
    let (_, outcome) = started(f.run("r1", "hello", None).await?)?;
    ensure!(reply_text(&outcome) == "Reply to: hello", "reply: {:?}", outcome.reply);
    ensure!(matches!(outcome.finish, Finish::Done));
    let agent = f.agent();
    let kinds = agent.kinds(from);
    ensure!(kinds.first().map(String::as_str) == Some("agent.step.v1"), "journal: {kinds:?}");
    ensure!(kinds.last().map(String::as_str) == Some("agent.run-result.v1"), "journal: {kinds:?}");
    Ok(())
}

/// A repeated run id is refused as a duplicate: nothing written, no call.
async fn duplicate(f: &Fixture) -> Result<()> {
    let before = f.agent().entries.len();
    let chats = f.agent().chats.len();
    match f.run("r1", "echo again", None).await? {
        Ran::Refused(StartError::Duplicate) => {}
        Ran::Refused(e) => bail!("expected duplicate, got {e:?}"),
        Ran::Started { .. } => bail!("a repeated run id started"),
    }
    ensure!(f.agent().entries.len() == before, "a duplicate wrote to the session");
    ensure!(f.agent().chats.len() == chats, "a duplicate called the model");
    Ok(())
}

/// Dropping the event stream after the first event cancels the run: the
/// agent stops at its next write, never runs the tool the model asked for,
/// leaves no op open, and the journal ends with agent.run-result.v1
/// cancelled.
async fn cancelled(f: &Fixture) -> Result<()> {
    let calls = f.agent().tool_calls.len();
    match f.run_reading("r-cancel", "echo stop me", None, Some(1)).await? {
        Ran::Started { outcome: Err(RunError::Cancelled), events } => ensure!(events.len() == 1, "{} events read", events.len()),
        Ran::Started { outcome, .. } => bail!("expected cancelled, got {outcome:?}"),
        Ran::Refused(e) => bail!("did not start: {e:?}"),
    }
    let agent = f.agent();
    ensure!(agent.tool_calls.len() == calls, "the tool ran after the driver went away");
    let last = agent.entries.last().context("no journal")?;
    ensure!(last.kind == "agent.run-result.v1", "last entry {}", last.kind);
    ensure!(last.json() == json!({"run": "r-cancel", "outcome": "cancelled", "error": "cancelled"}), "{}", last.data);
    let mut open = std::collections::BTreeSet::new();
    for entry in &agent.entries {
        match entry.kind.as_str() {
            "agent.op.v1" => {
                open.insert(entry.json()["op"].to_string());
            }
            "agent.op-result.v1" => {
                open.remove(&entry.json()["op"].to_string());
            }
            _ => {}
        }
    }
    ensure!(open.is_empty(), "ops left open: {open:?}");
    Ok(())
}

/// Three more exchanges, so there is something to summarize.
async fn more_runs(f: &Fixture) -> Result<()> {
    for (id, topic) in [("r2", "comets"), ("r3", "moons"), ("r4", "rings")] {
        let text = padded(topic);
        let (_, outcome) = started(f.run(id, &text, None).await?)?;
        ensure!(reply_text(&outcome) == format!("Reply to: {text}"), "reply to {id}: {:?}", outcome.reply);
    }
    Ok(())
}

/// The driver asks for a summary between runs, with a focus: the model is
/// sent the summary request and the person's focus as their own message
/// right after it, never as instructions; agent.compaction.v1 (trigger
/// requested), the smaller state and agent.compact-result.v1 land in one
/// commit after the op's result; the events are compacted then counts; the
/// outcome is done with no reply.
async fn requested(f: &Fixture, covers: Covers) -> Result<()> {
    let focus = "keep what was said about moons";
    let from = f.agent().entries.len();
    let chats = f.agent().chats.len();
    let bytes = state_bytes(f);
    let covered = f.agent().state.as_ref().and_then(|s| s.through).context("no state")?;
    let through = match covers {
        Covers::All => covered,
        Covers::BeforeLatestExchange => before_last_person(&f.agent().entries, covered).context("no person's step")?,
    };
    let (events, outcome) = started(f.compact("c1", Some(focus), |_| {}).await?)?;
    println!("    events: {}", events.iter().map(describe).collect::<Vec<_>>().join(", "));
    ensure!(matches!(&events[..], [RunEvent::Compacted(_), RunEvent::Counts(_)]), "compacted then counts");
    ensure!(matches!(outcome.finish, Finish::Done) && outcome.reply.is_empty() && outcome.structured.is_none());
    ensure!(outcome.counts.steps == 1 && outcome.counts.tool_calls == 0, "{:?}", outcome.counts);
    ensure!(outcome.model.as_ref().is_some_and(|m| m.model == host::MODEL), "model: {:?}", outcome.model);

    let agent = f.agent();
    ensure!(agent.chats.len() == chats + 1, "{} summary calls", agent.chats.len() - chats);
    let request = &agent.chats[chats];
    ensure!(request.tools.is_empty(), "a summary offers no tools: {:?}", request.tools);
    let at = host::summary_request(&request.messages).context("not a summary request")?;
    ensure!(
        request.messages.get(at + 1).and_then(host::user_text).as_deref() == Some(focus),
        "the focus is the person's own message right after the request"
    );
    ensure!(
        !request.messages.iter().any(|m| matches!(m, Message::System(s) if s.contains(focus))),
        "the focus is never instructions"
    );

    let kinds = agent.kinds(from);
    println!("    journal: {}", kinds.join(" "));
    ensure!(
        kinds == ["agent.compact.v1", "agent.op.v1", "agent.op-result.v1", "agent.compaction.v1", "agent.compact-result.v1"],
        "journal kinds"
    );
    ensure!(agent.entries[from].json() == json!({"compact": "c1", "focus": focus}), "{}", agent.entries[from].json());
    let op = agent.entries[from + 1].json();
    ensure!(op["target"] == "model" && op["purpose"] == "compaction", "{op}");
    let op_result = agent.entries[from + 2].json();
    ensure!(op_result == json!({"op": op["op"], "outcome": "succeeded"}), "{op_result}");
    let entry = agent.entries[from + 3].json();
    println!("    compaction: kept={} tokens {} -> {}, through {}", entry["kept"], entry["tokensBefore"], entry["tokensAfter"], entry["through"]);
    ensure!(entry["trigger"] == "requested" && entry["focus"] == focus, "{entry}");
    ensure!(entry["through"] == json!({"start": 0, "end": through}), "through {}", entry["through"]);
    if covers == Covers::BeforeLatestExchange {
        ensure!(entry["kept"] == 2, "the latest exchange is kept: {}", entry["kept"]);
    }
    ensure!(entry["summary"].as_str().is_some_and(|s| s.contains(focus)), "summary {}", entry["summary"]);
    let result = agent.entries[from + 4].json();
    ensure!(result == json!({"compact": "c1", "outcome": "done", "finish": "done", "compacted": true}), "{result}");
    ensure!(agent.state.as_ref().and_then(|s| s.through) == Some(agent.entries[from + 4].seq), "one commit with the state");

    let RunEvent::Compacted(c) = events[0] else { unreachable!() };
    ensure!(
        matches!(c.trigger, CompactionTrigger::Requested)
            && c.through.start == 0
            && c.through.end == through
            && u64::from(c.kept) == entry["kept"].as_u64().unwrap_or_default()
            && c.seq == Some(agent.entries[from + 3].seq)
            && c.tokens_before.is_some_and(|b| c.tokens_after.is_some_and(|a| a < b)),
        "compacted event: {c:?}"
    );
    drop(agent);
    let after = state_bytes(f);
    println!("    state: {bytes} bytes -> {after}");
    ensure!(after < bytes, "the state did not shrink");
    Ok(())
}

/// The next run reads the summary, and never a reply it summarized; an
/// agent that keeps the latest exchange sends that exchange, and none of the
/// person's earlier messages.
async fn after_requested(f: &Fixture, covers: Covers) -> Result<()> {
    let (_, outcome) = started(f.run("r5", "and after the summary?", None).await?)?;
    ensure!(reply_text(&outcome) == "Reply to: and after the summary?", "reply: {:?}", outcome.reply);
    let agent = f.agent();
    let sent = format!("{:?}", agent.chats.last().context("no model call")?.messages);
    ensure!(sent.contains("SUMMARY:"), "the summary is not in the next run's request");
    ensure!(!sent.contains("Reply to: Tell me about comets"), "a summarized reply was sent again");
    if covers == Covers::BeforeLatestExchange {
        ensure!(sent.contains("Tell me about rings"), "the kept exchange is not in the next run's request");
        ensure!(!sent.contains("Tell me about comets"), "a summarized message was sent again");
    }
    Ok(())
}

/// The same compaction id again is a duplicate: nothing written, no call.
/// A run may still use the same id: a namespace apart.
async fn requested_again(f: &Fixture) -> Result<()> {
    let before = f.agent().entries.len();
    let chats = f.agent().chats.len();
    match f.compact("c1", Some("anything"), |_| {}).await? {
        Ran::Refused(StartError::Duplicate) => {}
        Ran::Refused(e) => bail!("expected duplicate, got {e:?}"),
        Ran::Started { .. } => bail!("a repeated compaction id started"),
    }
    ensure!(f.agent().entries.len() == before, "a duplicate wrote to the session");
    ensure!(f.agent().chats.len() == chats, "a duplicate called the model");
    let (_, outcome) = started(f.run("c1", "a run named like a compaction", None).await?)?;
    ensure!(reply_text(&outcome) == "Reply to: a run named like a compaction", "reply: {:?}", outcome.reply);
    Ok(())
}

/// A compaction asked for while a run is in progress on the same instance is
/// refused busy, writing nothing; the run is unaffected.
async fn requested_busy(f: &Fixture) -> Result<()> {
    let (compacting, ran) = f.compact_during_run("r-busy", "c-busy").await?;
    ensure!(matches!(compacting, Err(StartError::Busy)), "expected busy, got {compacting:?}");
    let (_, outcome) = started(ran)?;
    ensure!(reply_text(&outcome) == "Reply to: a run in progress", "reply: {:?}", outcome.reply);
    let agent = f.agent();
    ensure!(
        !agent.entries.iter().any(|e| e.kind == "agent.compact.v1" && e.json()["compact"] == "c-busy"),
        "the busy compaction was accepted"
    );
    Ok(())
}

/// A summary the model refuses is never committed: its op closes as failed
/// with the compaction's result, the state is unchanged, and the compaction
/// fails without a compacted event.
async fn requested_refused(f: &Fixture) -> Result<()> {
    // Something to summarize again.
    started(f.run("r6", &padded("asteroids"), None).await?)?;
    let from = f.agent().entries.len();
    let version = f.agent().state.as_ref().map(|s| s.version.clone());
    match f.compact("c-refused", Some(host::REFUSE_SUMMARY), |_| {}).await? {
        Ran::Started { events, outcome: Err(RunError::Failed(m)) } => {
            ensure!(m.contains("refused"), "{m}");
            ensure!(!events.iter().any(|e| matches!(e, RunEvent::Compacted(_))), "a compacted event");
        }
        Ran::Started { outcome, .. } => bail!("expected failed, got {outcome:?}"),
        Ran::Refused(e) => bail!("did not start: {e:?}"),
    }
    let agent = f.agent();
    let kinds = agent.kinds(from);
    ensure!(kinds == ["agent.compact.v1", "agent.op.v1", "agent.op-result.v1", "agent.compact-result.v1"], "journal: {kinds:?}");
    ensure!(agent.entries[from + 2].json()["outcome"] == "failed");
    let result = agent.entries[from + 3].json();
    ensure!(result == json!({"compact": "c-refused", "outcome": "failed", "error": "failed", "compacted": false}), "{result}");
    ensure!(agent.state.as_ref().map(|s| s.version.clone()) == version, "the state changed");
    Ok(())
}

/// A limit already spent stops a compaction before its call: done with that
/// finish, and nothing summarized.
async fn requested_limit(f: &Fixture) -> Result<()> {
    let chats = f.agent().chats.len();
    let (events, outcome) = started(f.compact("c-limit", None, |o| o.max_total_tokens = Some(0)).await?)?;
    ensure!(events.is_empty(), "events: {}", events.len());
    ensure!(matches!(outcome.finish, Finish::MaxTotalTokens), "{:?}", outcome.finish);
    ensure!(f.agent().chats.len() == chats, "the summary call was made");
    let result = f.agent().entries.last().context("no journal")?.json();
    ensure!(result["finish"] == "max-total-tokens" && result["compacted"] == false, "{result}");
    Ok(())
}

/// Dropping a compaction's events while its summary call is still running
/// stops the call within a heartbeat, not when the model answers: nothing is
/// committed and the compaction is recorded as cancelled.
async fn compaction_dropped_mid_summary(f: &Fixture) -> Result<()> {
    let from = f.agent().entries.len();
    f.agent().reply_delay_ms = 5000;
    let began = std::time::Instant::now();
    let ran = f.compact_reading("c-dropped", None, |_| {}, Some(0)).await;
    f.agent().reply_delay_ms = 0;
    match ran? {
        Ran::Started { outcome: Err(RunError::Cancelled), .. } => {}
        Ran::Started { outcome, .. } => bail!("expected cancelled, got {outcome:?}"),
        Ran::Refused(e) => bail!("did not start: {e:?}"),
    }
    ensure!(began.elapsed() < std::time::Duration::from_secs(4), "the summary ran to the end: {:?}", began.elapsed());
    let kinds = f.agent().kinds(from);
    ensure!(!kinds.iter().any(|k| k == "agent.compaction.v1"), "a summary was committed: {kinds:?}");
    let agent = f.agent();
    let result = agent.entries[from..].iter().rev().find(|e| e.kind == "agent.compact-result.v1").context("no compact result")?;
    ensure!(result.json()["outcome"] == "cancelled", "{}", result.data);
    Ok(())
}

/// A tool call whose outcome is unknown ends the run with an error. Its op
/// closes as unresolved with an error step naming it, the run result says
/// failed, and nothing else is called: no second model call, and the tool is
/// never asked again. The saved state stays as it was before the reply that
/// asked for the call. Run on a fresh session.
pub(crate) async fn unknown_outcome_stops(f: &Fixture) -> Result<()> {
    f.agent().unknown_outcome = true;
    match f.run("r-unknown", "echo once only", None).await? {
        Ran::Started { outcome: Err(RunError::Failed(m)), .. } => {
            ensure!(m.contains("call-1"), "the error does not name the call: {m}")
        }
        Ran::Started { outcome, .. } => bail!("expected the run to fail, got {outcome:?}"),
        Ran::Refused(e) => bail!("did not start: {e:?}"),
    }
    let agent = f.agent();
    ensure!(agent.chats.len() == 1, "{} model calls; the run went on after the call", agent.chats.len());
    ensure!(agent.tool_calls.len() == 1, "{} tool calls; the call was made again", agent.tool_calls.len());
    let results: Vec<serde_json::Value> =
        agent.entries.iter().filter(|e| e.kind == "agent.op-result.v1").map(Entry::json).collect();
    ensure!(
        results.len() == 1 && results[0]["op"] == "call-1" && results[0]["outcome"] == "unresolved",
        "op results: {results:?}"
    );
    let at = agent.entries.iter().position(|e| e.kind == "agent.op-result.v1").context("no op result")?;
    ensure!(
        agent.entries[at + 1..].iter().any(|e| e.kind == "agent.step.v1" && e.json()["role"] == "error"),
        "no error step after the unresolved call"
    );
    let result = agent.entries.last().context("empty journal")?.json();
    ensure!(result["run"] == "r-unknown" && result["outcome"] == "failed", "{result}");
    let state = agent.state.as_ref().context("no saved state")?;
    ensure!(
        !String::from_utf8_lossy(&state.value).contains("call-1"),
        "the saved state moved past the unresolved call"
    );
    Ok(())
}
