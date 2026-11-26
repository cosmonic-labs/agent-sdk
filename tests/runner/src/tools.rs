//! `cosmonic:agent/tools` as the agent sees it: the granted tools, each call
//! checked against the grants, recorded, and answered by a stub or a real
//! tool component on its own fresh instance.
use std::sync::Arc;

use wasmtime::Store;
use wasmtime::component::{Accessor, HasSelf};

use crate::host::{Host, Offered, Source, ToolComponent, tool_bindings, tool_types, tools, types};
use crate::record::{Dispatched, Refusal, Refused, Response};
use tool_types::{CallError, DispatchError, OutcomeError, ToolResponse};
use types::{ToolArguments, ToolCall, ToolOutput};

/// A fresh instance of a tool component, with its own grants.
async fn instance(
    host: &Arc<Host>,
    tool: &ToolComponent,
) -> anyhow::Result<(Store<crate::host::State>, tool_bindings::ToolUnderTest)> {
    let state = host.state(&tool.name, &tool.hosts, &tool.folders)?;
    let mut store = Store::new(&host.engine, state);
    crate::runner::set_deadline(&mut store, host.timeout);
    let inst = tool_bindings::ToolUnderTest::instantiate_async(
        &mut store,
        &tool.component,
        &host.tool_linker,
    )
    .await?;
    Ok((store, inst))
}

/// What a tool component offers.
pub async fn list_component(
    host: &Arc<Host>,
    tool: &ToolComponent,
) -> anyhow::Result<Vec<tool_types::ToolDef>> {
    let (mut store, inst) = instance(host, tool).await?;
    let listed = store
        .run_concurrent(async |acc| inst.cosmonic_agent_tools().call_list(acc).await)
        .await??;
    listed.map_err(|e| anyhow::anyhow!("{} could not list its tools: {e:?}", tool.name))
}

async fn call_component(
    host: Arc<Host>,
    tool: Arc<ToolComponent>,
    request: ToolCall,
) -> anyhow::Result<Result<ToolResponse, CallError>> {
    let (mut store, inst) = instance(&host, &tool).await?;
    Ok(store
        .run_concurrent(async |acc| inst.cosmonic_agent_tools().call_call(acc, request).await)
        .await??)
}

fn summarize(r: &Result<ToolResponse, CallError>) -> Response {
    match r {
        Ok(resp) => Response::Returned {
            text: resp
                .output
                .iter()
                .filter_map(|o| match o {
                    ToolOutput::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            is_error: resp.is_error,
        },
        Err(e) => Response::Failed(format!("{e:?}")),
    }
}

impl tools::Host for crate::host::State {}

impl<T: Send> tools::HostWithStore<T> for HasSelf<crate::host::State> {
    async fn list(
        store: &Accessor<T, Self>,
    ) -> wasmtime::Result<Result<Vec<tool_types::ToolDef>, tool_types::ListError>> {
        let host = store.with(|mut s| Arc::clone(&s.get().host));
        Ok(Ok(host
            .tools()
            .iter()
            .filter(|o| host.is_granted(&o.def.name))
            .map(|o| o.def.clone())
            .collect()))
    }

    async fn call(
        store: &Accessor<T, Self>,
        request: ToolCall,
    ) -> wasmtime::Result<Result<ToolResponse, CallError>> {
        let host = store.with(|mut s| Arc::clone(&s.get().host));
        let known = host.tools().iter().find(|o| o.def.name == request.name);
        let refuse = |why: &str| {
            host.record().refused.push(Refused {
                by: "agent".into(),
                what: Refusal::Tool {
                    name: request.name.clone(),
                    why: why.into(),
                },
            });
        };
        let Some(Offered { source, .. }) = known else {
            refuse("unknown");
            return Ok(Err(CallError::NotDispatched(DispatchError::UnknownTool(
                request.name.clone(),
            ))));
        };
        if !host.is_granted(&request.name) {
            refuse("not granted");
            return Ok(Err(CallError::NotDispatched(DispatchError::Denied(
                format!("{} is not granted to this agent", request.name),
            ))));
        }
        let (arguments, text) = match &request.arguments {
            ToolArguments::Json(j) => match serde_json::from_str(j) {
                Ok(v) => (Some(v), None),
                Err(_) => (None, Some(j.clone())),
            },
            ToolArguments::Text(t) => (None, Some(t.clone())),
        };
        // Recorded before it runs, so a call the agent abandons (cancelled,
        // or dropped) still shows as dispatched.
        let slot = {
            let mut rec = host.record();
            rec.dispatched.push(Dispatched {
                id: request.id.clone(),
                name: request.name.clone(),
                arguments,
                text,
                response: Response::Failed("unfinished: the agent stopped waiting".into()),
            });
            rec.dispatched.len() - 1
        };
        let result = match source {
            Source::Stub(results) => {
                let n = {
                    let mut calls = host.stub_calls();
                    let n = calls.entry(request.name.clone()).or_default();
                    *n += 1;
                    *n
                };
                match results.get(n - 1).or(results.last()) {
                    Some(r) => r.clone(),
                    None => Ok(ToolResponse {
                        output: vec![],
                        structured: None,
                        is_error: false,
                    }),
                }
            }
            Source::Component(tool) => {
                // Aborted if the agent drops this call, so nothing keeps
                // running behind the test's back.
                let mut task = AbortOnDrop(tokio::spawn(call_component(
                    Arc::clone(&host),
                    Arc::clone(tool),
                    request.clone(),
                )));
                match (&mut task.0).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(e)) => {
                        host.record()
                            .problems
                            .push(format!("tool {} trapped: {e:#}", tool.name));
                        Err(CallError::OutcomeUnknown(OutcomeError::Other(
                            e.to_string(),
                        )))
                    }
                    Err(e) => {
                        host.record()
                            .problems
                            .push(format!("tool {} panicked: {e}", tool.name));
                        Err(CallError::OutcomeUnknown(OutcomeError::Other(
                            e.to_string(),
                        )))
                    }
                }
            }
        };
        if let Some(d) = host.record().dispatched.get_mut(slot) {
            d.response = summarize(&result);
        }
        Ok(result)
    }
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
