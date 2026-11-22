# agent-test

Deterministic tests for agents built on `cosmonic:agent`. A test is a YAML file that names an agent component, what it is granted, a scripted model and the tools it may call. The runner hosts the agent, drives one run through its `run` export, records everything the host did on the agent's behalf, and checks the test's assertions against that record.

No model is called and nothing reaches a network, so the same test gives the same result on a laptop and in CI, with no keys.

> Status: a first version of the format and runner. The format is versioned (`version: 1`) so it can change without breaking files written against it.

## Checked against grants, not an LLM judge

An agent's reply is text a model wrote. It can say a tool was never called when it was, or that a request was blocked when it went through. So these tests never ask a model whether an agent behaved. They check facts the host recorded:

- every tool call the host dispatched, with its arguments and the response the agent got back;
- every call or request the grants refused, and which component asked;
- every outbound HTTP request any component made, let through or not;
- how many model calls the host served, and the session journal the agent wrote.

`never: {calls: [delete_file]}` passes only if the host never dispatched `delete_file`, whatever the model says. [`must-fail/model-says-otherwise.yaml`](must-fail/model-says-otherwise.yaml) shows a run where the model claims it deleted nothing; the test fails because the host's record says otherwise.

The model is scripted, so what these tests exercise is the agent harness (the loop around the model) and the grants around it: does the harness pass the model's calls through faithfully, handle refusals and failures, stop when it should, and record what it did; and does a tool reach only what it was granted.

## Run

Build the runner once. It needs the `cosmonic:agent` WIT, fetched with [`wkg`](https://github.com/bytecodealliance/wasm-pkg-tools):

```shell
cd testing/agent-test
WKG_CONFIG_FILE=../../wit/wkg-registries.toml wkg wit fetch
cargo build --release
```

Then run a file or a directory of them:

```shell
target/release/agent-test ../../components/minimal-agent/agent-tests
```

Each file prints one line. A failing test lists every way the record differs from what it expects; here, an agent build that ignored its tool grants:

```text
ok   a granted tool is called with the model's arguments (.../01-echo.yaml)
FAIL a tool that is not granted is refused and never runs (.../02-ungranted-tool.yaml)
       tool calls: expected exactly [], the host dispatched: delete_file({"path":"/home/me/notes.txt"}) -> "deleted"
       refusals: expected 1 in order, the host recorded: none
       never: delete_file was dispatched 1 times
4 passed, 1 failed
```

`--agent <component.wasm>` runs every file against that component instead of the one each file names, so one suite can test a build from CI. From Rust, call `agent_test::run_file(path, agent)`.

The examples in [`components/minimal-agent/agent-tests`](../../components/minimal-agent/agent-tests) need minimal-agent and the [`fetch-tool`](fixtures/fetch-tool) fixture built first (`cargo build --target wasm32-wasip2 --release` in each, after the same `wkg wit fetch`).

## The format

```yaml
version: 1
name: a tool that is not granted is refused and never runs
agent:
  component: ../target/wasm32-wasip2/release/minimal_agent.wasm
grants:
  tools: [echo]
tools:
  - name: echo
    results:
      - output: hello
  - name: delete_file
    results:
      - output: deleted
model:
  replies:
    - tool-calls:
        - name: delete_file
          arguments: {path: /home/me/notes.txt}
    - text: I could not delete it.
run:
  message: tidy up my notes
expect:
  outcome: done
  tool-calls: []
  refused:
    - tool: delete_file
      by: agent
  never:
    calls: [delete_file]
```

Paths are relative to the test file. Unknown fields are errors, so a typo never turns into an assertion that silently checks nothing.

### `agent` and `grants`

| Field | Meaning |
| --- | --- |
| `agent.component` | The agent component. It must export `cosmonic:agent/run`. |
| `grants.tools` | The tools the host offers and dispatches. Left out, every tool under `tools` is granted. A call to a declared tool that is not granted is refused as `denied`; a call to an undeclared tool as `unknown-tool`. Both are recorded. |
| `grants.hosts` | Hosts the agent component itself may send HTTP requests to: `api.example.com`, `api.example.com:8443`, or `*.example.com` for any subdomain. |
| `grants.folders` | Folders the agent component may open: `{path, guest, writable}`. |

Sockets are never granted. An attempt is refused and recorded.

### `tools`

A tool is a stub or a real tool component.

A stub has a `name` and answers from `results`, one per call in order (the last one repeats):

```yaml
- name: deploy
  description: Deploy the app
  input: {type: object, properties: {env: {type: string}}}   # or `text` for a freeform tool
  read-only: false
  results:
    - output: deployed to staging
    - output: quota exceeded
      is-error: true
    - error: {not-dispatched: timed-out}
    - error: {outcome-unknown: {upstream: connection reset}}
```

A tool component exports `cosmonic:agent/tools` and is hosted on a fresh instance for each call, with its own grants, as it would be in a workload. It is linked with WASI and HTTP behind its grants, never with the model, the session or other tools:

```yaml
- component: fixtures/fetch-tool.wasm
  hosts: [api.example.com]
  folders:
    - {path: files, guest: /data}
```

A granted HTTP request gets its response from the top-level `http` list. A granted request with no stub fails the test, so a test never depends on a network:

```yaml
http:
  - url: https://api.example.com/status
    method: GET          # the default
    status: 200          # the default
    body: '{"ok": true}'
```

### `model`

Each model call gets the next reply. Running out of replies fails the test.

```yaml
model:
  alias: default            # the alias the host serves; others are refused
  replies:
    - reasoning: Planning the call.
      text: Using the tool.
      tool-calls:
        - name: echo
          arguments: {text: hello}       # JSON arguments, as YAML
        - name: shout
          text: quiet words              # freeform arguments
          id: call-shout                 # default: call-1, call-2, ...
    - text: Done.
      finish: stop                       # default: tool-calls if any, else stop
    - request-error: rate-limited        # or context-length-exceeded, timed-out
```

### `run`

| Field | Default |
| --- | --- |
| `message` | Required. |
| `id` | `run-1` |
| `instructions` | none |
| `schema` | none. A JSON Schema, as YAML. |
| `max-steps` | `8` |
| `timeout` | `60` seconds |

### `expect`

Every field is optional; `outcome` defaults to `done`.

| Field | Passes when |
| --- | --- |
| `outcome` | The run ended this way: `done`, `failed`, `cancelled`, `model-refused`, `model-failed`, `output-rejected`, or `not-started`. |
| `finish` | A run that replied finished this way: `done`, `max-steps`, `refusal`, `input-required`, ... |
| `reply` | The reply's text matches: a string (exact), or `{contains, starts-with, matches}` (`matches` is a regular expression). |
| `tool-calls` | The calls the host dispatched. A list means exactly these, in order. `{in-order: [...]}` allows others between; `{includes: [...]}` allows any order. Each call matches on `name`, and optionally `arguments` (equal), `arguments-include` (these fields, recursively), `text`, `is-error` and `result` (the response's text). |
| `call-counts` | `{tool: n}`: how many times each tool was dispatched. |
| `refused` | Every refusal the host recorded, in order: `{tool}` or `{host}` (a socket refusal is `{host: <ip>:<port>}`), with optional `by` (`agent`, or the tool component's file name without `.wasm`). `[]` means none. |
| `never.calls` | These tools were never dispatched. |
| `never.reaches` | No component's request to these hosts was let through. Written like a grant: `host`, `host:port` or `*.domain`. |
| `model-calls` | The host served this many model calls. |
| `journal` | The session journal's entry kinds, in order. |

## Why YAML

Workload manifests are YAML, so a test's grants read like the workload they stand for. A script is deeply nested (replies hold tool calls that hold arguments), which YAML writes plainly and TOML's arrays of tables do not. Tool arguments and schemas are JSON written as YAML, with no escaping.

## Limits

- The record covers the whole test, including listing tool components before the run.
- Every store has a deadline of `run.timeout` plus five seconds, enforced by epoch interruption, so a guest stuck in a loop traps instead of hanging the runner.
- One run per test. A second run that continues the session is future work.
- Folder access is preopens only: a path outside the granted folders does not exist for the component, so there is nothing to refuse and nothing is recorded. Only the tool's own response shows the failed read.
- A held approval (a host asking a person before a tool call) is not modeled. A stub can return `{not-dispatched: {denied: ...}}` to stand for a person saying no.
- The session's journal is not replayed. Its entries record what the agent did, not the model's full replies or the tools' full responses, so a recorded journal cannot be turned into a script on its own.
- Tool components are hosted through `cosmonic:agent/tools`. Typed tools (an ordinary WIT interface a host translates) are not.
- `cosmonic:agent` 0.4.2. Unstable features from later versions (session status, spawn) are not served.

## Future work

- **Save a real session as a test.** A host already sees every model reply, tool call, response and refusal. Writing those out in this format turns a session someone ran into a regression test, with its grants.
- **A CI step.** A small action that builds the runner once and runs a repository's tests against the component it just built.
- **A promotion gate.** A clustered host could run an agent's tests against a new build before rolling it out, and refuse a build whose record shows a call or a request its grants should have stopped.
- **More runs per test, held approvals, typed tools and recorded journals,** as the limits above say.
