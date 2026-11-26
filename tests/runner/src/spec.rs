//! Inputs for a single run of an agent component.
use std::path::PathBuf;

use serde_json::Value;

use crate::host::tool_types::{CallError, ToolDef, ToolResponse};
use crate::host::types::{FinishReason, RequestError, ToolArguments};

pub struct Scenario {
    pub agent: PathBuf,
    pub message: String,
    pub replies: Vec<Reply>,
    pub grants: Grants,
    pub tools: Vec<Tool>,
    pub http: Vec<HttpStub>,
    pub alias: String,
    pub run_id: String,
    pub instructions: Option<String>,
    pub schema: Option<Value>,
    pub max_steps: u32,
    /// Wall-clock deadline in seconds.
    pub timeout: u64,
}

impl Scenario {
    pub fn new(agent: impl Into<PathBuf>, message: &str, replies: Vec<Reply>) -> Self {
        Self {
            agent: agent.into(),
            message: message.into(),
            replies,
            grants: Grants::default(),
            tools: vec![],
            http: vec![],
            alias: "default".into(),
            run_id: "run-1".into(),
            instructions: None,
            schema: None,
            max_steps: 8,
            timeout: 60,
        }
    }
}

#[derive(Default)]
pub struct Grants {
    /// None offers all declared tools. An empty list offers none.
    pub tools: Option<Vec<String>>,
    pub hosts: Vec<String>,
    pub folders: Vec<Folder>,
}

#[derive(Clone)]
pub struct Folder {
    pub path: PathBuf,
    pub guest: String,
    pub writable: bool,
}

pub enum Tool {
    Stub {
        def: ToolDef,
        /// One response per call. The last repeats if the agent calls again.
        results: Vec<Result<ToolResponse, CallError>>,
    },
    Component {
        path: PathBuf,
        hosts: Vec<String>,
        folders: Vec<Folder>,
    },
}

#[derive(Clone)]
pub struct HttpStub {
    pub method: String,
    pub url: String,
    pub status: u16,
    pub body: String,
}

#[derive(Clone, Default)]
pub struct Reply {
    pub reasoning: Option<String>,
    pub text: Option<String>,
    pub refusal: Option<String>,
    pub tool_calls: Vec<Call>,
    pub finish: Option<FinishReason>,
    pub request_error: Option<RequestError>,
}

impl Reply {
    pub fn text(text: &str) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    pub fn calls(calls: Vec<Call>) -> Self {
        Self {
            tool_calls: calls,
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub struct Call {
    /// Defaults to call-N, counting across all replies.
    pub id: Option<String>,
    pub name: String,
    pub arguments: ToolArguments,
}

impl Call {
    pub fn json(name: &str, arguments: Value) -> Self {
        Self {
            id: None,
            name: name.into(),
            arguments: ToolArguments::Json(arguments.to_string()),
        }
    }
}
