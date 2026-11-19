//! The workload's tools: what the host offers, making a call the model asks
//! for, and its output as text.

use crate::agent::inference_types::{
    AssistantContent, AssistantMessage, Attachment, ToolCall, ToolDef, ToolOutput, ToolResult,
};
use crate::agent::tool_types::{CallError, ListError, OutcomeError};
use crate::agent::tools;
use crate::conversation::StoredResult;
use crate::describe::Describe;
use crate::events::Events;
use crate::journal::{
    Journal, OP_KIND, OP_RESULT_KIND, Op, OpResult, STEP_KIND, Step, arguments_value, entry,
};

/// What the host offers: the tools as the model sees them, and what each
/// one's provider says about being read only.
#[derive(Default)]
pub(crate) struct Offered {
    pub(crate) defs: Vec<ToolDef>,
    read_only: Vec<(String, Option<bool>)>,
}

impl Offered {
    /// The `read-only` annotation of the tool by this name: `Some(true)` only
    /// when every tool offered by that name says so, and `None` when none is
    /// offered by that name.
    fn read_only(&self, name: &str) -> Option<bool> {
        let mut flags = self.read_only.iter().filter(|(n, _)| n == name);
        let first = flags.next()?.1;
        flags.fold(first, |all, (_, flag)| match (all, flag) {
            (Some(true), Some(true)) => Some(true),
            (Some(true), other) => *other,
            (all, _) => all,
        })
    }
}

/// What the host offers, described to the model as the provider describes
/// it. Empty when the workload grants no tools; an error when listing failed,
/// which the run records before it carries on without tools.
pub(crate) async fn offered_tools() -> Result<Offered, ListError> {
    let mut offered = Offered::default();
    for t in tools::list().await? {
        offered
            .read_only
            .push((t.name.clone(), t.annotations.read_only));
        offered.defs.push(ToolDef {
            name: t.name,
            title: t.title,
            description: t.description,
            input: t.input,
            output_schema: t.output_schema,
        });
    }
    Ok(offered)
}

/// Whether a call error stops the run. An unknown outcome does, so the run
/// never goes past a call that may have taken effect, with one exception: a
/// timeout on a tool whose provider says it is read only. That claim is taken
/// at its word (it is the provider's, unchecked), so the model reads the
/// timeout and can ask for less. A missing `read-only` annotation counts as
/// not read only.
fn stops_the_run(error: &CallError, read_only: Option<bool>) -> bool {
    match error {
        CallError::NotDispatched(_) => false,
        CallError::OutcomeUnknown(OutcomeError::TimedOut) => read_only != Some(true),
        CallError::OutcomeUnknown(_) => true,
    }
}

/// How one tool call ended, for the run.
pub(crate) enum Called {
    Done(ToolResult),
    /// The journal moved on under this run; it stops before the next call.
    Superseded,
    /// The driver dropped the event stream after the call returned.
    Gone,
    /// Whether the call took effect is unknown. The run stops here, before
    /// the model can ask for it again and before the stored conversation
    /// moves past it: a person resolves it.
    Unresolved(String),
}

/// Make one tool call the model asked for, recorded around the dispatch: the
/// op before, so a restart knows it may have happened, and the result after.
pub(crate) async fn call_tool(
    events: &mut Events,
    journal: &mut Journal,
    offered: &Offered,
    call: &ToolCall,
    run: &str,
) -> Called {
    let op = Op {
        op: &call.id,
        target: "tool",
        name: &call.name,
        arguments: Some(arguments_value(&call.arguments)),
    };
    if journal.append(vec![entry(OP_KIND, &op)]).await.is_err() {
        return Called::Superseded;
    }

    let mut unresolved = None;
    let (output, is_error, outcome) = match tools::call(call.clone()).await {
        Ok(r) if r.is_error => (r.output, true, "failed"),
        Ok(r) => (r.output, false, "succeeded"),
        // Not made: the model gets the reason, so it can try again.
        Err(e @ CallError::NotDispatched(_)) => {
            (vec![ToolOutput::Text(e.describe())], true, "failed")
        }
        // Made, or perhaps made, and whether it took effect is not known.
        // The session conventions say never to advance state past it.
        Err(e) if stops_the_run(&e, offered.read_only(&call.name)) => {
            let why = e.describe();
            unresolved = Some(why.clone());
            (vec![ToolOutput::Text(why)], true, "unresolved")
        }
        // A read-only tool that timed out: nothing to resolve, so it is
        // closed as failed and the model reads the timeout.
        Err(e) => (vec![ToolOutput::Text(e.describe())], true, "failed"),
    };
    let text = output_text(&output);
    let names = [call.name.as_str()];
    let response = Step {
        role: "tool",
        text: &text,
        blocks: &["toolResponse"],
        tools: &names,
        run: Some(run),
    };
    let entries = vec![
        entry(
            OP_RESULT_KIND,
            &OpResult {
                op: &call.id,
                outcome,
            },
        ),
        entry(STEP_KIND, &response),
    ];
    let seq = journal.record(entries).await;
    let result = ToolResult {
        call_id: call.id.clone(),
        output,
        is_error,
    };
    if !events.tools(vec![result.clone()], seq).await {
        return Called::Gone;
    }
    match unresolved {
        Some(why) => Called::Unresolved(format!(
            "the {} tool call {} stopped the run: {why}. It was not made again; check \
             whether it took effect before asking again",
            call.name, call.id
        )),
        None => Called::Done(result),
    }
}

/// A tool call the run will not make because its time limit passed: no op,
/// since nothing was dispatched, but a tool step and an error result, so the
/// person sees why and the stored conversation has a result for every call.
pub(crate) async fn skip_tool(
    events: &mut Events,
    journal: &mut Journal,
    call: &ToolCall,
    run: &str,
) -> Called {
    let text = "Not called: the run's time limit passed.";
    let names = [call.name.as_str()];
    let response = Step {
        role: "tool",
        text,
        blocks: &["toolResponse"],
        tools: &names,
        run: Some(run),
    };
    let seq = journal.record(vec![entry(STEP_KIND, &response)]).await;
    let result = ToolResult {
        call_id: call.id.clone(),
        output: vec![ToolOutput::Text(text.to_string())],
        is_error: true,
    };
    if !events.tools(vec![result.clone()], seq).await {
        return Called::Gone;
    }
    Called::Done(result)
}

/// The tool calls in a reply, in order.
pub(crate) fn tool_calls(message: &AssistantMessage) -> Vec<ToolCall> {
    message
        .content
        .iter()
        .filter_map(|part| match &part.content {
            AssistantContent::ToolUse(call) => Some(call.clone()),
            _ => None,
        })
        .collect()
}

/// A tool's output as text: the model sees attachments themselves, the
/// journal and the stored conversation a note naming each one.
fn output_text(output: &[ToolOutput]) -> String {
    output
        .iter()
        .map(|o| match o {
            ToolOutput::Text(text) => text.clone(),
            ToolOutput::Attachment(Attachment { media_type, .. }) => {
                format!("[{media_type} attachment]")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn stored_result(result: &ToolResult) -> StoredResult {
    StoredResult {
        call_id: result.call_id.clone(),
        text: output_text(&result.output),
        is_error: result.is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool_types::DispatchError;

    fn offered(tools: &[(&str, Option<bool>)]) -> Offered {
        Offered {
            defs: vec![],
            read_only: tools.iter().map(|(n, r)| (n.to_string(), *r)).collect(),
        }
    }

    #[test]
    fn a_read_only_tool_that_timed_out_does_not_stop_the_run() {
        let timed_out = CallError::OutcomeUnknown(OutcomeError::TimedOut);
        assert!(!stops_the_run(&timed_out, Some(true)));
    }

    #[test]
    fn a_tool_that_is_not_read_only_stops_the_run_when_it_times_out() {
        let timed_out = CallError::OutcomeUnknown(OutcomeError::TimedOut);
        assert!(stops_the_run(&timed_out, Some(false)));
    }

    #[test]
    fn a_tool_with_no_read_only_annotation_stops_the_run_when_it_times_out() {
        let timed_out = CallError::OutcomeUnknown(OutcomeError::TimedOut);
        assert!(stops_the_run(&timed_out, None));
        // Nor does a tool the host never offered count as read only.
        assert_eq!(offered(&[("grep", Some(true))]).read_only("rm"), None);
    }

    #[test]
    fn other_unknown_outcomes_stop_the_run_even_for_a_read_only_tool() {
        for e in [
            OutcomeError::Upstream("x".into()),
            OutcomeError::Cancelled,
            OutcomeError::Other("x".into()),
        ] {
            assert!(stops_the_run(&CallError::OutcomeUnknown(e), Some(true)));
        }
        let never = CallError::NotDispatched(DispatchError::TimedOut);
        assert!(!stops_the_run(&never, None));
    }

    #[test]
    fn a_name_is_read_only_only_if_every_tool_by_it_says_so() {
        assert_eq!(offered(&[("a", Some(true))]).read_only("a"), Some(true));
        for other in [Some(false), None] {
            let both = offered(&[("a", Some(true)), ("a", other)]);
            assert_ne!(both.read_only("a"), Some(true));
            let both = offered(&[("a", other), ("a", Some(true))]);
            assert_ne!(both.read_only("a"), Some(true));
        }
    }
}
