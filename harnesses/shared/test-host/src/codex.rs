//! The Codex harness's own cases beyond the shared contract suite: a tool
//! turn with continuation data, the plan tool, freeform input, refusals,
//! pre-turn compaction at 90 percent of the window, structured output, and
//! the history a requested summary leaves.

use anyhow::{Context as _, Result, bail, ensure};

use crate::fixture::*;
use crate::host::types::{AssistantContent, Message};
use crate::host::run_types::{Finish, RunError, RunEvent, StepContent};
use crate::host::{self, Agent};

/// One turn with a tool call: streamed deltas, the call through the host's
/// tools import, its output back to the model, the continuation data of the
/// first reply replayed verbatim, and the journal around it all.
pub(crate) async fn tool_turn(f: &Fixture) -> Result<()> {
    let (events, outcome) = started(f.run("r1", "echo hello from codex", None).await?)?;
    for ev in &events {
        println!("    {}", describe(ev));
    }
    ensure!(reply_text(&outcome) == "The tool said: hello from codex", "reply: {:?}", outcome.reply);
    ensure!(matches!(outcome.finish, Finish::Done));
    ensure!(outcome.counts.steps == 2 && outcome.counts.tool_calls == 1, "counts {:?}", outcome.counts);
    ensure!(outcome.model.as_ref().is_some_and(|m| m.model == host::MODEL));
    ensure!(events.iter().any(|e| matches!(e, RunEvent::ReasoningDelta(_))), "no reasoning delta");
    ensure!(events.iter().any(|e| matches!(e, RunEvent::TextDelta(t) if t == "Using the tool.")), "no text delta");
    ensure!(
        events.iter().any(|e| matches!(e, RunEvent::Step(s) if matches!(&s.content, StepContent::Tool(r) if r[0].call_id == "call-1"))),
        "no tool step"
    );

    let agent = f.agent();
    ensure!(agent.tool_calls.len() == 1 && agent.tool_calls[0].name == "echo", "host tool calls: {:?}", agent.tool_calls);
    ensure!(agent.chats.len() == 2, "{} model calls", agent.chats.len());
    let first = &agent.chats[0];
    ensure!(first.tools == ["echo", "explode", "patch", "update_plan"], "tools offered: {:?}", first.tools);
    ensure!(matches!(&first.messages[0], Message::System(s) if s == "Be brief."), "driver instructions first");
    ensure!(matches!(&first.messages[1], Message::System(s) if s.starts_with("You are Codex")), "Codex's base instructions second");
    // The second request replays the first reply, continuation data intact.
    let second = &agent.chats[1];
    let Some(Message::Assistant(replayed)) = second.messages.iter().rev().nth(1) else { bail!("no replayed reply") };
    let opaque: Vec<Option<&str>> = replayed.content.iter().map(|p| p.opaque.as_deref()).collect();
    ensure!(opaque == [Some("enc-1"), None, Some("sig-1")], "replayed continuation data: {opaque:?}");
    ensure!(matches!(second.messages.last(), Some(Message::Tool(r)) if r[0].call_id == "call-1" && !r[0].is_error));

    let kinds = agent.kinds(0);
    println!("    journal: {}", kinds.join(" "));
    ensure!(
        kinds == [
            "agent.step.v1", "agent.run.v1",
            "agent.step.v1", "agent.op.v1",
            "agent.op-result.v1", "agent.step.v1",
            "agent.step.v1",
            "agent.run-result.v1"
        ],
        "journal kinds"
    );
    let op_result = agent.entries.iter().find(|e| e.kind == "agent.op-result.v1").unwrap().json();
    ensure!(op_result["op"] == "call-1" && op_result["outcome"] == "succeeded", "{op_result}");
    let result = agent.entries.last().unwrap().json();
    ensure!(result["run"] == "r1" && result["outcome"] == "done" && result["finish"] == "done", "{result}");
    let history = agent.history();
    let types: Vec<&str> = history.iter().map(|i| i["type"].as_str().unwrap_or("?")).collect();
    println!("    saved history: {}", types.join(" "));
    ensure!(types == ["message", "reasoning", "message", "function_call", "function_call_output", "message"]);
    ensure!(history[1]["encrypted_content"] == "enc-1" && history[3]["x_cosmonic_opaque"] == "sig-1");
    Ok(())
}

/// The next run, on a fresh instance, continues from the saved history.
pub(crate) async fn second_turn(f: &Fixture) -> Result<()> {
    let (_, outcome) = started(f.run("r2", "and now plain text", None).await?)?;
    ensure!(reply_text(&outcome) == "Reply to: and now plain text", "reply: {:?}", outcome.reply);
    let agent = f.agent();
    let last = agent.chats.last().unwrap();
    // 3 system (with the note that every tool here only reads) + first run
    // (user, assistant, tool, assistant) + this user message.
    ensure!(last.messages.len() == 8, "the model saw {} messages", last.messages.len());
    ensure!(
        format!("{:?}", last.messages).contains("every tool you have only reads"),
        "the read-only note is missing"
    );
    ensure!(agent.history().len() == 8);
    Ok(())
}

/// A call to a tool the host does not offer never reaches the host; the model
/// gets Codex's "unsupported call" output and the journal gets no op.
pub(crate) async fn unknown_tool(f: &Fixture) -> Result<()> {
    let calls = f.agent().tool_calls.len();
    let from = f.agent().entries.len();
    let (_, outcome) = started(f.run("r3", "call missing", None).await?)?;
    ensure!(reply_text(&outcome) == "The tool said: unsupported call: missing", "reply: {:?}", outcome.reply);
    let agent = f.agent();
    ensure!(agent.tool_calls.len() == calls, "the host was asked to run an unknown tool");
    ensure!(!agent.kinds(from).iter().any(|k| k == "agent.op.v1"), "an op was journaled for a call never made");
    Ok(())
}

/// `update_plan` is answered by the agent itself: the plan is journaled as
/// `agent.plan.v1`, then streams as the `plan` run event, the model reads
/// "Plan updated", and neither the host's tools nor the journal's ops see the
/// call.
pub(crate) async fn plan(f: &Fixture) -> Result<()> {
    let calls = f.agent().tool_calls.len();
    let from = f.agent().entries.len();
    let (events, outcome) = started(f.run("r-plan", "plan the fix", None).await?)?;
    let plans: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Plan(entries) => Some(entries),
            _ => None,
        })
        .collect();
    ensure!(plans.len() == 1, "{} plan events", plans.len());
    println!("    {}", describe(&RunEvent::Plan(plans[0].clone())));
    use host::run_types::PlanStatus;
    let statuses: Vec<PlanStatus> = plans[0].iter().map(|e| e.status).collect();
    ensure!(
        plans[0][1].content == "Fix the bug"
            && matches!(statuses[..], [PlanStatus::Completed, PlanStatus::InProgress, PlanStatus::Pending]),
        "{statuses:?}"
    );
    ensure!(reply_text(&outcome) == "The tool said: Plan updated", "reply: {:?}", outcome.reply);
    ensure!(outcome.counts.tool_calls == 1);
    let agent = f.agent();
    ensure!(agent.tool_calls.len() == calls, "the plan reached the host's tools");
    ensure!(!agent.kinds(from).iter().any(|k| k == "agent.op.v1"), "an op was journaled for the plan");
    let saved: Vec<(usize, &host::Entry)> =
        agent.entries.iter().enumerate().skip(from).filter(|(_, e)| e.kind == "agent.plan.v1").collect();
    ensure!(saved.len() == 1, "{} agent.plan.v1 entries", saved.len());
    let (at, entry) = saved[0];
    let data: serde_json::Value = serde_json::from_str(&entry.data)?;
    ensure!(data["run"] == "r-plan", "plan entry: {data}");
    let statuses: Vec<&str> = data["entries"].as_array().into_iter().flatten().filter_map(|e| e["status"].as_str()).collect();
    ensure!(statuses == ["completed", "in-progress", "pending"], "plan entry statuses: {statuses:?}");
    ensure!(data["entries"][1]["content"] == "Fix the bug", "plan entry: {data}");
    let seen_at = *agent.plan_events_at.last().context("no plan event was read")?;
    ensure!(at < seen_at, "the plan event streamed before agent.plan.v1 was committed");
    Ok(())
}

/// A freeform tool (Codex's apply_patch shape) is offered with its format and
/// grammar, and its raw input reaches the host byte for byte: not parsed as
/// JSON, not answered with a parse error, and journaled as a JSON string.
pub(crate) async fn freeform(f: &Fixture) -> Result<()> {
    let calls = f.agent().tool_calls.len();
    let from = f.agent().entries.len();
    let (_, outcome) = started(f.run("r-patch", "patch the greeting", None).await?)?;
    let expected = format!("The tool said: applied {} bytes", host::PATCH.len());
    ensure!(reply_text(&outcome) == expected, "reply: {:?}", outcome.reply);
    let agent = f.agent();
    ensure!(agent.tool_calls.len() == calls + 1, "the patch did not reach the host");
    let call = agent.tool_calls.last().unwrap();
    ensure!(
        call.name == "patch" && matches!(&call.arguments, host::types::ToolArguments::Text(t) if t == host::PATCH),
        "arguments changed: {:?}",
        call.arguments
    );
    let offered = agent.chats.last().unwrap();
    let i = offered.tools.iter().position(|t| t == "patch").context("patch not offered")?;
    ensure!(
        matches!(&offered.inputs[i], host::types::ToolInput::Grammar(g)
            if matches!(g.syntax, host::types::GrammarSyntax::Lark) && g.definition == host::PATCH_GRAMMAR),
        "input: {:?}",
        offered.inputs[i]
    );
    let echo = offered.tools.iter().position(|t| t == "echo").context("echo not offered")?;
    ensure!(
        matches!(&offered.inputs[echo], host::types::ToolInput::JsonSchema(s) if s.contains("\"required\"")),
        "echo input: {:?}",
        offered.inputs[echo]
    );
    // The call goes back to the model, in the same run's next request, as
    // the text it wrote.
    let replayed = offered.messages.iter().any(|m| {
        matches!(m, host::types::Message::Assistant(a) if a.content.iter().any(|p| matches!(&p.content,
            AssistantContent::ToolUse(c) if matches!(&c.arguments, host::types::ToolArguments::Text(t) if t == host::PATCH))))
    });
    ensure!(replayed, "the patch call did not go back to the model as text");
    let op = agent.entries[from..].iter().find(|e| e.kind == "agent.op.v1").context("no op for the patch")?.json();
    ensure!(op["name"] == "patch" && op["arguments"] == host::PATCH, "{op}");
    Ok(())
}

pub(crate) async fn refused(f: &Fixture) -> Result<()> {
    let from = f.agent().entries.len();
    match f.run("r4", "refuse please", None).await? {
        Ran::Started { outcome: Err(RunError::ModelRefused(_)), .. } => {}
        Ran::Started { outcome, .. } => bail!("expected model-refused, got {outcome:?}"),
        Ran::Refused(e) => bail!("did not start: {e:?}"),
    }
    let agent = f.agent();
    let result = agent.entries.last().unwrap().json();
    ensure!(result["outcome"] == "failed" && result["error"] == "model-refused", "{result}");
    ensure!(agent.kinds(from).iter().filter(|k| *k == "agent.step.v1").count() == 2, "user and error steps");
    Ok(())
}

/// With a small context window, the history is over 90% of it before the
/// next run samples, so Codex's pre-turn compaction runs first: the model is
/// sent the compaction prompt, and the history becomes the person's recent
/// messages plus the summary.
pub(crate) async fn compaction(f: &Fixture) -> Result<()> {
    f.agent().context_length = Some(400);
    let from = f.agent().entries.len();
    let chats = f.agent().chats.len();
    let (events, outcome) = started(f.run("r5", "after compaction", None).await?)?;
    let agent = f.agent();
    ensure!(outcome.counts.steps == 2, "one compaction call and one reply: {:?}", outcome.counts);
    // The summary call sits between its op and op-result, and its result
    // lands with agent.compaction.v1 in one commit.
    let kinds = agent.kinds(from);
    println!("    journal: {}", kinds.join(" "));
    ensure!(
        kinds[..6] == ["agent.step.v1", "agent.run.v1", "agent.op.v1", "agent.op-result.v1", "agent.compaction.v1", "agent.step.v1"],
        "journal kinds"
    );
    let op = agent.entries[from + 2].json();
    ensure!(op["target"] == "model" && op["purpose"] == "compaction", "{op}");
    let op_result = agent.entries[from + 3].json();
    ensure!(op_result["op"] == op["op"] && op_result["outcome"] == "succeeded", "{op_result}");
    let person = agent.entries[from].seq;
    let compaction_request = &agent.chats[chats];
    ensure!(compaction_request.tools.is_empty(), "compaction offers no tools");
    ensure!(
        compaction_request.messages.last().and_then(host::user_text).is_some_and(|t| t.contains(host::SUMMARIZATION_MARKER)),
        "the compaction prompt is last"
    );
    let entry = agent.entries[from..].iter().find(|e| e.kind == "agent.compaction.v1").context("no compaction entry")?.json();
    println!("    compaction: trigger={} kept={} tokens {} -> {}", entry["trigger"], entry["kept"], entry["tokensBefore"], entry["tokensAfter"]);
    ensure!(entry["trigger"] == "threshold");
    // It covers the journal up to the person's message, which it keeps.
    ensure!(entry["through"] == serde_json::json!({"start": 0, "end": person - 1}), "through: {}", entry["through"]);
    let seq = agent.entries[from + 4].seq;
    let compacted: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Compacted(c) => Some(*c),
            _ => None,
        })
        .collect();
    ensure!(compacted.len() == 1, "{} compacted events", compacted.len());
    let c = compacted[0];
    ensure!(
        matches!(c.trigger, host::run_types::CompactionTrigger::Threshold)
            && c.through.start == 0
            && c.through.end == person - 1
            && c.seq == Some(seq)
            && u64::from(c.kept) == entry["kept"].as_u64().unwrap_or_default(),
        "compacted event: {c:?}"
    );
    let at = events.iter().position(|e| matches!(e, RunEvent::Compacted(_)));
    let reply = events.iter().position(|e| matches!(e, RunEvent::TextDelta(_) | RunEvent::Step(_)));
    ensure!(at < reply, "compacted is streamed before the reply: {:?}", events.iter().map(describe).collect::<Vec<_>>());
    let history = agent.history();
    let texts: Vec<String> = history
        .iter()
        .map(|i| i["content"][0]["text"].as_str().unwrap_or_default().chars().take(40).collect())
        .collect();
    println!("    history after: {texts:?}");
    ensure!(history.iter().all(|i| i["type"] == "message"), "only messages survive compaction");
    ensure!(texts.iter().any(|t| t.starts_with("Another language model")), "the summary is in the history");
    ensure!(reply_text(&outcome) == "Reply to: after compaction", "reply: {:?}", outcome.reply);
    ensure!(texts.last().is_some_and(|t| t.starts_with("Reply to: after")) && texts[texts.len() - 2] == "after compaction", "the new message follows the summary");
    f_reset_context(agent);
    Ok(())
}

fn f_reset_context(mut agent: std::sync::MutexGuard<'_, Agent>) {
    agent.context_length = None;
}

pub(crate) async fn structured_output(f: &Fixture) -> Result<()> {
    let schema = r#"{"type":"object"}"#;
    match f.run("r6", "not json", Some(schema)).await? {
        Ran::Started { outcome: Err(RunError::OutputRejected(_)), .. } => {}
        Ran::Started { outcome, .. } => bail!("expected output-rejected, got {outcome:?}"),
        Ran::Refused(e) => bail!("did not start: {e:?}"),
    }
    let agent = f.agent();
    ensure!(matches!(&agent.chats.last().unwrap().response_format, host::types::ResponseFormat::JsonSchema(s) if s == schema));
    Ok(())
}

/// A requested summary leaves Codex's history as the person's messages and
/// then the summary, the focus being the summary request's last message:
/// the shape of the saved state the next run replays. The rest of the
/// compaction's contract is the shared suite's.
pub(crate) async fn requested_history(f: &Fixture) -> Result<()> {
    let focus = "keep the echo results";
    let chats = f.agent().chats.len();
    let history_before = f.agent().history().len();
    started(f.compact("c1", Some(focus), |_| {}).await?)?;
    let agent = f.agent();
    ensure!(agent.chats.len() == chats + 1, "one summary call");
    ensure!(
        agent.chats[chats].messages.last().and_then(host::user_text).as_deref() == Some(focus),
        "the focus is the person's own message, last"
    );
    let history = agent.history();
    println!("    history: {history_before} items -> {}", history.len());
    ensure!(history.len() < history_before, "the history did not shrink");
    ensure!(history.iter().all(|i| i["type"] == "message" && i["role"] == "user"), "only the person's messages and the summary");
    let last = history.last().and_then(|i| i["content"][0]["text"].as_str()).unwrap_or_default();
    ensure!(last.starts_with("Another language model") && last.contains(focus), "the summary is last: {last:?}");
    Ok(())
}
