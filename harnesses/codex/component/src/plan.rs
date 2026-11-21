//! Codex's plan tool, `update_plan`, handled in the component. Ported from
//! openai/codex 0.162.0 (c1382380), `codex-rs/core/src/tools/handlers/plan.rs`
//! and `codex-rs/protocol/src/plan_tool.rs`, Apache-2.0.
//!
//! Upstream the plan is a tool whose only effect is an event the client
//! draws; the model gets "Plan updated" back. Here that event is the 0.4.0
//! `plan` run event. The call never goes to the host's `tools.call`, and gets
//! no op in the journal, since nothing outside the agent runs. The plan itself
//! is journaled as `agent.plan.v1` before the event streams.

use serde::Deserialize;

use crate::bindings::cosmonic::agent::inference_types as llm;
use crate::bindings::cosmonic::agent::run_types::{PlanEntry, PlanStatus};
use crate::bindings::cosmonic::agent::tool_types as tt;

pub const NAME: &str = "update_plan";

/// Upstream's tool description, verbatim.
const DESCRIPTION: &str = "Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.
";

const SCHEMA: &str = r#"{"type":"object","properties":{"explanation":{"type":"string"},"plan":{"type":"array","description":"The list of steps","items":{"type":"object","properties":{"step":{"type":"string"},"status":{"type":"string","description":"One of: pending, in_progress, completed"}},"required":["step","status"],"additionalProperties":false}}},"required":["plan"],"additionalProperties":false}"#;

/// What the model reads back, as upstream.
pub const OUTPUT: &str = "Plan updated";

pub fn tool_def() -> tt::ToolDef {
    tt::ToolDef {
        name: NAME.into(),
        title: None,
        description: DESCRIPTION.into(),
        input: llm::ToolInput::JsonSchema(SCHEMA.into()),
        output_schema: None,
        annotations: tt::ToolAnnotations {
            read_only: Some(true),
            destructive: Some(false),
            idempotent: Some(true),
            open_world: Some(false),
        },
        version: None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    #[serde(default)]
    #[allow(dead_code)]
    explanation: Option<String>,
    plan: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    step: String,
    status: Status,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    InProgress,
    Completed,
}

/// The plan the model sent, or the error text it reads back. The explanation
/// is the model's note to itself; upstream does not draw it either.
pub fn parse(arguments: &str) -> Result<Vec<PlanEntry>, String> {
    let args: Args =
        serde_json::from_str(arguments).map_err(|e| format!("failed to parse function arguments: {e}"))?;
    if args.plan.iter().filter(|i| i.status == Status::InProgress).count() > 1 {
        return Err("at most one step can be in_progress at a time".into());
    }
    Ok(args
        .plan
        .into_iter()
        .map(|i| PlanEntry {
            content: i.step,
            status: match i.status {
                Status::Pending => PlanStatus::Pending,
                Status::InProgress => PlanStatus::InProgress,
                Status::Completed => PlanStatus::Completed,
            },
        })
        .collect())
}

/// The `agent.plan.v1` entry for a plan.
pub fn entry(plan: &[PlanEntry], run: &str) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = plan
        .iter()
        .map(|e| {
            serde_json::json!({
                "content": e.content,
                "status": match e.status {
                    PlanStatus::Pending => "pending",
                    PlanStatus::InProgress => "in-progress",
                    PlanStatus::Completed => "completed",
                },
            })
        })
        .collect();
    serde_json::json!({ "entries": entries, "run": run })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_parses_in_order() {
        let plan = parse(r#"{"explanation":"x","plan":[{"step":"read","status":"completed"},{"step":"fix","status":"in_progress"},{"step":"test","status":"pending"}]}"#)
            .unwrap();
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[1].content, "fix");
        assert!(matches!(plan[1].status, PlanStatus::InProgress));
    }

    #[test]
    fn bad_plans_are_answered_not_drawn() {
        assert!(parse(r#"{"plan":[{"step":"a","status":"done"}]}"#).is_err());
        assert!(parse(r#"{"plan":[{"step":"a","status":"in_progress"},{"step":"b","status":"in_progress"}]}"#).is_err());
        assert!(parse(r#"{"steps":[]}"#).is_err());
    }

    #[test]
    fn the_entry_uses_the_interface_status_names() {
        let plan = parse(r#"{"plan":[{"step":"a","status":"in_progress"},{"step":"b","status":"pending"}]}"#).unwrap();
        let e = entry(&plan, "r1");
        assert_eq!(e["run"], "r1");
        assert_eq!(e["entries"][0]["status"], "in-progress");
        assert_eq!(e["entries"][1]["content"], "b");
    }

    #[test]
    fn the_schema_is_json() {
        serde_json::from_str::<serde_json::Value>(SCHEMA).unwrap();
    }
}
