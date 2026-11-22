//! The test file format, version 1. See README.md for the reference.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

/// One test: an agent, what it is granted, a scripted model, the tools, one
/// run and what the host must have recorded.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Test {
    /// The format version. Only `1` exists.
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub agent: Agent,
    #[serde(default)]
    pub grants: Grants,
    #[serde(default)]
    pub tools: Vec<Tool>,
    /// Responses for granted outbound HTTP requests. Nothing reaches a network.
    #[serde(default)]
    pub http: Vec<HttpStub>,
    pub model: Model,
    pub run: Run,
    #[serde(default)]
    pub expect: Expect,
    /// Where the file was read from; relative paths resolve against it.
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Agent {
    /// The agent component, relative to the test file. The runner's
    /// `--agent` flag overrides it.
    pub component: String,
}

/// What the agent component is granted. Each real tool component carries its
/// own `hosts` and `folders`, as it would in a workload.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Grants {
    /// The tools the host offers and dispatches, by name. Left out, every tool
    /// under `tools` is granted. A call to any other tool is refused.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// The hosts the agent component may send HTTP requests to.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// The folders the agent component may open.
    #[serde(default)]
    pub folders: Vec<Folder>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Folder {
    /// A directory on the machine running the test, relative to the test file.
    pub path: String,
    /// Where the component sees it.
    pub guest: String,
    #[serde(default)]
    pub writable: bool,
}

/// A stubbed tool (`name`) or a real tool component (`component`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Tool {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// A JSON Schema (as YAML), or `text` for a freeform tool. Default:
    /// `{type: object}`.
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub read_only: Option<bool>,
    /// The stub's results, one per call in order; the last one repeats.
    #[serde(default)]
    pub results: Vec<StubResult>,
    /// A component that exports `cosmonic:agent/tools`, relative to the test
    /// file. It offers whatever its `list` returns.
    #[serde(default)]
    pub component: Option<String>,
    /// The hosts that tool component may send HTTP requests to.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// The folders that tool component may open.
    #[serde(default)]
    pub folders: Vec<Folder>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct StubResult {
    /// Text returned to the agent.
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub structured: Option<Value>,
    /// The tool ran and failed.
    #[serde(default)]
    pub is_error: bool,
    /// The call failed instead of returning a response, e.g.
    /// `{not-dispatched: timed-out}` or `{outcome-unknown: {upstream: gone}}`.
    #[serde(default, deserialize_with = "via_json")]
    pub error: Option<CallErrorSpec>,
}

/// A field that is present, even as `null`, is `Some`: `arguments: null`
/// means null arguments, never "don't check".
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// YAML's enum syntax is tags (`!not-dispatched`); read these as the plain
/// maps and strings a reader expects, the way JSON spells an enum.
fn via_json<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let v = Option::<Value>::deserialize(d)?;
    v.map(|v| serde_json::from_value(v).map_err(serde::de::Error::custom))
        .transpose()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallErrorSpec {
    NotDispatched(DispatchSpec),
    OutcomeUnknown(OutcomeSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispatchSpec {
    UnknownTool(String),
    InvalidArguments(String),
    Upstream(String),
    Denied(String),
    Cancelled,
    TimedOut,
    Other(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutcomeSpec {
    Upstream(String),
    Cancelled,
    TimedOut,
    Other(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct HttpStub {
    #[serde(default = "get")]
    pub method: String,
    /// The full URL, e.g. `https://api.example.com/v1/status`.
    pub url: String,
    #[serde(default = "ok")]
    pub status: u16,
    #[serde(default)]
    pub body: String,
}

fn get() -> String {
    "GET".into()
}

fn ok() -> u16 {
    200
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Model {
    /// The alias the host serves. Any other alias is refused.
    #[serde(default = "default_alias")]
    pub alias: String,
    /// One reply per model call, in order.
    pub replies: Vec<Reply>,
}

fn default_alias() -> String {
    "default".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Reply {
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub refusal: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<CallSpec>,
    /// Default: `tool-calls` when the reply has any, `stop` otherwise.
    #[serde(default)]
    pub finish: Option<String>,
    /// Refuse the request instead of replying: `rate-limited`,
    /// `context-length-exceeded`, `timed-out` or `other`.
    #[serde(default)]
    pub request_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CallSpec {
    /// Default: `call-<n>`, counting every call in the script from 1.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    /// JSON arguments, written as YAML.
    #[serde(default, deserialize_with = "present")]
    pub arguments: Option<Value>,
    /// Freeform arguments, for a `text` tool. Exclusive with `arguments`.
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Run {
    pub message: String,
    #[serde(default = "run_id")]
    pub id: String,
    #[serde(default)]
    pub instructions: Option<String>,
    /// A JSON Schema the reply must satisfy, written as YAML.
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default = "max_steps")]
    pub max_steps: u32,
    /// Seconds before the runner gives up on the run.
    #[serde(default = "timeout")]
    pub timeout: u64,
}

fn run_id() -> String {
    "run-1".into()
}

fn max_steps() -> u32 {
    8
}

fn timeout() -> u64 {
    60
}

/// What must hold once the run ends, checked against what the host recorded.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Expect {
    /// `done`, `not-started`, or a run error case such as `failed`,
    /// `model-refused` or `cancelled`.
    #[serde(default)]
    pub outcome: Option<String>,
    /// The finish of a run that ended with a reply: `done`, `max-steps`,
    /// `refusal` and so on.
    #[serde(default)]
    pub finish: Option<String>,
    /// The reply's text.
    #[serde(default)]
    pub reply: Option<TextMatch>,
    /// The tool calls the host dispatched.
    #[serde(default)]
    pub tool_calls: Option<Calls>,
    /// How many times the host dispatched each tool.
    #[serde(default)]
    pub call_counts: BTreeMap<String, usize>,
    /// Every refusal the host recorded, in order.
    #[serde(default)]
    pub refused: Option<Vec<Refusal>>,
    #[serde(default)]
    pub never: Never,
    /// How many model calls the host served.
    #[serde(default)]
    pub model_calls: Option<usize>,
    /// The session journal's entry kinds, in order.
    #[serde(default)]
    pub journal: Option<Vec<String>>,
}

/// A plain string matches exactly.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TextMatch {
    Exact(String),
    Rule(TextRule),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TextRule {
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub starts_with: Option<String>,
    /// A regular expression that must match somewhere in the text.
    #[serde(default)]
    pub matches: Option<String>,
}

/// A plain list is `exactly`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Calls {
    Exactly(Vec<CallMatch>),
    Rule(CallsRule),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CallsRule {
    /// These calls and no others, in this order.
    #[serde(default)]
    pub exactly: Option<Vec<CallMatch>>,
    /// These calls in this order, others allowed between them.
    #[serde(default)]
    pub in_order: Option<Vec<CallMatch>>,
    /// These calls, in any order.
    #[serde(default)]
    pub includes: Option<Vec<CallMatch>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CallMatch {
    pub name: String,
    /// The JSON arguments, equal.
    #[serde(default, deserialize_with = "present")]
    pub arguments: Option<Value>,
    /// The JSON arguments contain these fields (recursively).
    #[serde(default, deserialize_with = "present")]
    pub arguments_include: Option<Value>,
    /// Freeform arguments.
    #[serde(default)]
    pub text: Option<TextMatch>,
    /// The response the agent got back was (or was not) an error.
    #[serde(default)]
    pub is_error: Option<bool>,
    /// The response's text.
    #[serde(default)]
    pub result: Option<TextMatch>,
}

/// One refusal: a tool call or a network request the grants did not allow.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Refusal {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    /// The component that asked: `agent`, or a tool component's file stem.
    #[serde(default)]
    pub by: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Never {
    /// Tools the host never dispatched.
    #[serde(default)]
    pub calls: Vec<String>,
    /// Hosts no request was ever let through to, by any component.
    #[serde(default)]
    pub reaches: Vec<String>,
}

impl Test {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, path.parent().unwrap_or(Path::new(".")))
            .with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str, dir: &Path) -> Result<Self> {
        // Check the version first, so a future version fails on its number
        // rather than on a field this runner does not know.
        #[derive(Deserialize)]
        struct Version {
            version: Option<u32>,
        }
        let v: Version = serde_yaml_ng::from_str(text)?;
        match v.version {
            Some(1) => {}
            Some(n) => bail!("version {n} is not supported; this runner reads version 1"),
            None => bail!("missing `version: 1`"),
        }
        let mut test: Test = serde_yaml_ng::from_str(text)?;
        test.dir = dir.to_path_buf();
        test.validate()?;
        Ok(test)
    }

    fn validate(&self) -> Result<()> {
        for tool in &self.tools {
            match (&tool.name, &tool.component) {
                (Some(_), None) => {
                    if !tool.hosts.is_empty() || !tool.folders.is_empty() {
                        bail!(
                            "a stubbed tool takes no hosts or folders; only a tool component does"
                        );
                    }
                }
                (None, Some(_)) => {
                    if tool.input.is_some() || !tool.results.is_empty() {
                        bail!("a tool component describes itself; give it no input or results");
                    }
                }
                _ => bail!("each tool needs exactly one of `name` (a stub) or `component`"),
            }
        }
        for reply in &self.model.replies {
            for call in &reply.tool_calls {
                if call.arguments.is_some() && call.text.is_some() {
                    bail!(
                        "tool call {}: give `arguments` or `text`, not both",
                        call.name
                    );
                }
            }
        }
        for r in self.expect.refused.iter().flatten() {
            if r.tool.is_some() == r.host.is_some() {
                bail!("each refusal names exactly one of `tool` or `host`");
            }
        }
        Ok(())
    }

    pub fn resolve(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_versions() {
        let err = Test::parse("version: 2\n", Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("version 2"), "{err}");
        assert!(Test::parse("name: x\n", Path::new(".")).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = "version: 1\nname: x\nagent: {component: a.wasm}\nmodel: {replies: []}\nrun: {message: hi}\nexpekt: {}\n";
        assert!(Test::parse(text, Path::new(".")).is_err());
    }

    #[test]
    fn reads_a_full_test() {
        let text = r#"
version: 1
name: full
agent: {component: a.wasm}
grants: {tools: [echo], hosts: [api.example.com]}
tools:
  - name: echo
    input: {type: object}
    results:
      - output: hi
      - error: {not-dispatched: timed-out}
      - error: {outcome-unknown: {upstream: gone}}
  - component: t.wasm
    hosts: ["*.example.com"]
http:
  - url: https://api.example.com/x
    body: ok
model:
  replies:
    - tool-calls: [{name: echo, arguments: {text: hi}}]
    - text: done
run: {message: go}
expect:
  outcome: done
  reply: {contains: done}
  tool-calls: [{name: echo, arguments-include: {text: hi}}]
  refused: [{tool: rm}, {host: evil.example.com, by: agent}]
  never: {calls: [rm], reaches: [evil.example.com]}
"#;
        let t = Test::parse(text, Path::new("/t")).unwrap();
        assert_eq!(t.tools.len(), 2);
        assert!(matches!(
            t.tools[0].results[1].error,
            Some(CallErrorSpec::NotDispatched(DispatchSpec::TimedOut))
        ));
        assert!(matches!(t.expect.tool_calls, Some(Calls::Exactly(_))));
        assert_eq!(t.resolve("a.wasm"), PathBuf::from("/t/a.wasm"));
    }

    #[test]
    fn null_arguments_are_checked() {
        let m: CallMatch = serde_yaml_ng::from_str("{name: a, arguments: null}").unwrap();
        assert_eq!(m.arguments, Some(Value::Null));
        let m: CallMatch = serde_yaml_ng::from_str("{name: a}").unwrap();
        assert_eq!(m.arguments, None);
    }

    #[test]
    fn a_tool_is_a_stub_or_a_component() {
        let text = "version: 1\nname: x\nagent: {component: a.wasm}\ntools: [{name: a, component: b.wasm}]\nmodel: {replies: []}\nrun: {message: hi}\n";
        assert!(Test::parse(text, Path::new(".")).is_err());
    }
}
