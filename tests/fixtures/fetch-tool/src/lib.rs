//! A test fixture for agent-test: `fetch` GETs a URL and `read_file` reads a
//! file. Whether either works is up to the host's grants for this component.

mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "fetch-tool",
        generate_all,
    });
}

use bindings::cosmonic::agent::inference_types::{ToolArguments, ToolCall, ToolInput, ToolOutput};
use bindings::cosmonic::agent::tool_types::{
    CallError, DispatchError, ListError, ToolAnnotations, ToolDef, ToolResponse,
};
use bindings::exports::cosmonic::agent::tools::Guest;
use wasip3::http::types::{Fields, Method, Request, Scheme};
use wasip3::wit_bindgen::rt::async_support::StreamResult;
use wasip3::wit_future;

struct Tools;

fn def(name: &str, description: &str, field: &str) -> ToolDef {
    ToolDef {
        name: name.into(),
        title: None,
        description: description.into(),
        input: ToolInput::JsonSchema(format!(
            r#"{{"type":"object","properties":{{"{field}":{{"type":"string"}}}},"required":["{field}"]}}"#
        )),
        output_schema: None,
        annotations: ToolAnnotations {
            read_only: Some(true),
            destructive: Some(false),
            idempotent: Some(true),
            open_world: Some(name == "fetch"),
        },
        version: None,
    }
}

fn arg(call: &ToolCall, field: &str) -> Result<String, CallError> {
    let invalid = |why: &str| CallError::NotDispatched(DispatchError::InvalidArguments(why.into()));
    let ToolArguments::Json(json) = &call.arguments else {
        return Err(invalid("expected JSON arguments"));
    };
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| invalid(&e.to_string()))?;
    v[field]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| invalid(&format!("missing {field}")))
}

fn reply(text: String, is_error: bool) -> ToolResponse {
    ToolResponse {
        output: vec![ToolOutput::Text(text)],
        structured: None,
        is_error,
    }
}

async fn fetch(url: &str) -> Result<String, String> {
    let (scheme, rest) = match url.split_once("://") {
        Some(("https", r)) => (Scheme::Https, r),
        Some(("http", r)) => (Scheme::Http, r),
        _ => return Err(format!("not an http(s) URL: {url}")),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (trailers_tx, trailers_rx) = wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let (request, _sent) = Request::new(Fields::new(), None, trailers_rx, None);
    let bad = |what: &str| format!("could not build the request: {what}");
    request
        .set_method(&Method::Get)
        .map_err(|()| bad("method"))?;
    request
        .set_scheme(Some(&scheme))
        .map_err(|()| bad("scheme"))?;
    request
        .set_authority(Some(authority))
        .map_err(|()| bad("authority"))?;
    request
        .set_path_with_query(Some(path))
        .map_err(|()| bad("path"))?;
    let response = wasip3::http::client::send(request)
        .await
        .map_err(|e| format!("the request failed: {e:?}"))?;
    let status = response.get_status_code();
    let (done_tx, done_rx) = wit_future::new(|| Ok(()));
    let (mut stream, _trailers) = wasip3::http::types::Response::consume_body(response, done_rx);
    let mut body = Vec::new();
    loop {
        let (st, buf) = stream.read(Vec::with_capacity(4096)).await;
        body.extend_from_slice(&buf);
        match st {
            StreamResult::Complete(_) => continue,
            StreamResult::Dropped | StreamResult::Cancelled => break,
        }
    }
    drop(done_tx);
    Ok(format!("{status} {}", String::from_utf8_lossy(&body)))
}

impl Guest for Tools {
    async fn list() -> Result<Vec<ToolDef>, ListError> {
        Ok(vec![
            def("fetch", "GET a URL and return its status and body", "url"),
            def("read_file", "Read a text file", "path"),
        ])
    }

    async fn call(request: ToolCall) -> Result<ToolResponse, CallError> {
        match request.name.as_str() {
            "fetch" => {
                let url = arg(&request, "url")?;
                Ok(match fetch(&url).await {
                    Ok(text) => reply(text, false),
                    Err(e) => reply(e, true),
                })
            }
            "read_file" => {
                let path = arg(&request, "path")?;
                Ok(match std::fs::read_to_string(&path) {
                    Ok(text) => reply(text, false),
                    Err(e) => reply(format!("could not read {path}: {e}"), true),
                })
            }
            other => Err(CallError::NotDispatched(DispatchError::UnknownTool(
                other.into(),
            ))),
        }
    }
}

bindings::export!(Tools with_types_in bindings);
