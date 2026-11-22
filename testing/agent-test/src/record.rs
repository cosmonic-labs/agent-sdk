//! What the host saw during a run. Assertions are checked against this, never
//! against what the model said.
use serde_json::Value;

/// A tool call the host dispatched to a stub or a tool component.
#[derive(Debug, Clone)]
pub struct Dispatched {
    pub id: String,
    pub name: String,
    /// The JSON arguments, or `None` for freeform ones.
    pub arguments: Option<Value>,
    /// The freeform arguments, or the JSON as the model wrote it when it did
    /// not parse.
    pub text: Option<String>,
    /// What the agent got back.
    pub response: Response,
}

#[derive(Debug, Clone)]
pub enum Response {
    Returned { text: String, is_error: bool },
    Failed(String),
}

/// Something the grants did not allow.
#[derive(Debug, Clone)]
pub struct Refused {
    /// The component that asked: `agent`, or a tool component's name.
    pub by: String,
    pub what: Refusal,
}

#[derive(Debug, Clone)]
pub enum Refusal {
    /// A tool call to a tool that is not granted (or does not exist).
    Tool { name: String, why: String },
    /// An HTTP request to a host that is not granted.
    Host { host: String, url: String },
    /// A socket the component tried to open. No test grants sockets.
    Socket { addr: String },
}

/// An outbound HTTP request a component made, let through or not.
#[derive(Debug, Clone)]
pub struct Egress {
    pub by: String,
    pub method: String,
    pub url: String,
    pub host: String,
    /// The host with its port, when the URL gave one.
    pub authority: String,
    pub allowed: bool,
}

#[derive(Debug, Default)]
pub struct Record {
    pub dispatched: Vec<Dispatched>,
    pub refused: Vec<Refused>,
    pub egress: Vec<Egress>,
    /// Model calls served, with the tool names each one offered.
    pub model_calls: Vec<Vec<String>>,
    /// Faults in the test itself, such as a model called more often than the
    /// script has replies. Any of these fails the test.
    pub problems: Vec<String>,
}
