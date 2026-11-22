# Build your own agent harness

An agent harness is the loop at the center of an agent: it sends the conversation to a model, runs the tools the model asks for, records what happened and repeats until it has a reply. This guide builds one on `cosmonic:agent@0.4.2`, or ports an existing one (a Codex-like or Pi-like loop) onto it. The examples come from [`components/minimal-agent`](../components/minimal-agent), a reference harness in about 2,000 lines of Rust.

What you write is only the loop. The host makes the model calls, holds the keys, runs the tools and keeps the session. Your component starts with no network, no files and no credentials. [PORTABILITY.md](PORTABILITY.md) explains which of the rules below hold on every host.

## 1. Choose a world

A world is the list of what your component imports and exports. Pick one from [`world.wit`](../wit/cosmonic-agent/world.wit):

- **`runnable-agent`**: the `agent` imports plus the `run` export. Start here. A host drives your harness through `run`, so it needs no HTTP handler or protocol of its own.
- **`compactable-agent`**: `runnable-agent` plus `compact`, so a driver can ask for a summary between runs. Add it once you compact (step 6).
- **`agent`** alone: the imports with no export. Use it only if you serve another entry point yourself.

`run`, `run-types` and `compact` are unstable, behind the `agent-run` feature, so their shape may still change. Spelling the world out, as minimal-agent's [`wit/world.wit`](../components/minimal-agent/wit/world.wit) does, keeps the list a reviewer reads in one place:

```wit
world minimal-agent {
  import cosmonic:agent/inference-types@0.4.2;
  import cosmonic:agent/models@0.4.2;
  import cosmonic:agent/chat@0.4.2;
  import cosmonic:agent/session@0.4.2;
  import cosmonic:agent/tools@0.4.2;

  export cosmonic:agent/run@0.4.2;
}
```

Fetch the package with `wash wit fetch` (or `wkg`), pointing `WKG_CONFIG_FILE` at [`wit/wkg-registries.toml`](../wit/wkg-registries.toml). Commit the resulting `wkg.lock` so the dependency is pinned by digest.

## 2. Implement `run`

`run` must return quickly: once it has committed the person's message, it hands back a stream of events and a future for the outcome, and keeps working in the background. From minimal-agent's [`src/run.rs`](../components/minimal-agent/src/run.rs):

```rust
// Abridged.
async fn run(message: UserMessage, options: RunOptions) -> Result<StartedRun, WitStartError> {
    let req = validate(message, options).map_err(WitStartError::InvalidInput)?;
    let accepted = turn::accept(&req).await.map_err(|e| match e { /* busy, duplicate, unavailable */ })?;

    let (events_tx, events_rx) = wit_stream::new::<RunEvent>();
    let (outcome_tx, outcome_rx) = wit_future::new::<Result<RunOutcome, RunError>>(|| {
        Err(RunError::Failed("the agent stopped before the run ended".into()))
    });
    wit_bindgen::spawn_local(async move {
        let mut events = Events(events_tx);
        let ended = turn::run(&mut events, &req, accepted).await;
        drop(events); // the stream closes before the outcome resolves
        let _ = outcome_tx.write(ended.map(outcome).map_err(run_error)).await;
    });
    Ok((events_rx, outcome_rx))
}
```

What `accept` does, in order:

1. **Take the lock.** One run at a time per session. Return `busy` if a run is already in progress. The driver holds the real lock; yours is a backstop.
2. **Read the session** with `session.current()`, and close any ops a crashed run left open (step 5).
3. **Commit the opening:** the person's `agent.step.v1` and `agent.run.v1` (`{"run": "<run-id>"}`), with `commit-id` set to `run:<run-id>`. A retained duplicate id means `duplicate`.

Validate before writing anything. minimal-agent refuses an empty message, a `run-id` that would not fit the commit id, a schema that is not JSON, and attachments by URL, all as `invalid-input`.

Then the loop: call the model, run the tools it asks for, add the results, repeat. End the run with `agent.run-result.v1` (`run`, `outcome`, and `finish` or the error case) in the commit that ends it, then resolve the outcome. Stream a `step` event after each message you commit, `counts` after each model call, and stop when the driver drops the event stream (a failed write tells you).

Hold nothing in memory between runs. The next run may be a fresh instance; your conversation lives in the session's state and journal.

## 3. Call the model through the host

Open the model by alias, never by name. The host decides what `default` means for this workload:

```rust
let model = models::open("default".to_string()).await?;
```

`models.describe(alias)` reports what the host knows, including `context-length`, which you need for compaction.

`chat` takes the messages as a stream that you write while the call is pending, then a future that says the conversation is complete. From [`src/model.rs`](../components/minimal-agent/src/model.rs):

```rust
let (mut msg_tx, msg_rx) = wit_stream::new();
let (done_tx, done_rx) = wit_future::new(|| Err("the agent stopped writing".to_string()));
wit_bindgen::spawn_local(async move {
    let unwritten = msg_tx.write_all(messages).await;
    drop(msg_tx);
    let _ = done_tx.write(if unwritten.is_empty() { Ok(()) } else { Err("messages lost".into()) }).await;
});
let (mut chunks, outcome) = chat::chat(model, msg_rx, done_rx, options).await?;
while let Some(chunk) = chunks.next().await { /* stream text and reasoning deltas */ }
let completion = outcome.await?;
```

Drain the chunks before awaiting the outcome. Dropping the chunk stream cancels generation. Errors before acceptance are `request-error` (for example `context-length-exceeded`); errors after are `generation-error`. On `finish-reason` `pause-turn`, append the reply and call again. Pass the run's `effort` and `trace-parent` on in `chat-options`.

Record every model call in the journal: `agent.op.v1` (`target: "model"`, a fresh op id) before it, and `agent.op-result.v1` after.

## 4. Call tools

### List, offer, relay

List the tools at the start of each run and offer them to the model. `tools.list` returns `tool-types.tool-def`; `chat-options.tools` takes the `inference-types.tool-def` the model sees, which has no annotations or version. From [`src/tools.rs`](../components/minimal-agent/src/tools.rs):

```rust
for t in tools::list().await? {
    offered.read_only.push((t.name.clone(), t.annotations.read_only));
    offered.defs.push(ToolDef {
        name: t.name,
        title: t.title,
        description: t.description,
        input: t.input,
        output_schema: t.output_schema,
    });
}
```

If listing fails, record an `error` step and carry on without tools; don't read the failure as "no tools".

When the model replies with tool calls, pass each `tool-call` to `tools.call` exactly as it came: the same id, name and arguments. Don't parse or rebuild the arguments. A `json` argument is checked by the host against the tool's schema; a `text` argument (for a `grammar` or `text` tool) is the model's raw text and must never be parsed as JSON.

### Typed tools and where definitions come from

You never write tool definitions. A typed tool is a component that exports an ordinary WIT interface, such as [`components/units-tool`](../components/units-tool). The host reads that interface and builds a `tool-def` per function: the function name in snake case, its doc comment as the description, and, when its only parameter is a record, the record's fields as the arguments with their comments as argument descriptions. Because a built component keeps types but not comments, the descriptions come from the tool's published WIT package, built with `wasm-tools component wit wit --wasm -o units-wit.wasm`. The full rules are in the [design note](../wit/cosmonic-agent/README.md#what-makes-a-component-a-tool), and [Write a typed tool](https://cosmonic.com/docs/agent-sdk/write-a-typed-tool/) walks through one.

To your harness, a typed tool and a tool provider look the same: a `tool-def` in, a `tool-response` out. That is the point. You relay; the host translates.

### Record every call

Journal each tool call around its dispatch: `agent.op.v1` (`target: "tool"`, `op` set to the tool-call id, `arguments` as the model wrote them) committed before the call, and `agent.op-result.v1` plus the tool step after. The op must be in the journal before the call goes out, so a restart knows the call may have happened.

| `tools.call` result | `agent.op-result.v1` outcome | What the model reads |
| --- | --- | --- |
| `ok`, `is-error: false` | `succeeded` | The output |
| `ok`, `is-error: true` | `failed` | The output, as a normal result it can react to |
| `not-dispatched(...)` | `cancelled` in [`session.wit`](../wit/cosmonic-agent/session.wit)'s conventions | Why it didn't run; it may try again |
| `outcome-unknown(...)` | `unresolved` | See step 5 |

minimal-agent 0.3.2 records `not-dispatched` as `failed`. The session conventions reserve `failed` for a call that returned and use `cancelled` for one that was not dispatched, so prefer `cancelled`.

## 5. Handle unknown outcomes and timeouts

`outcome-unknown` means the call may have taken effect: the connection dropped after dispatch, the call was cancelled while running, or its deadline passed (`timed-out`). The email may have been sent; the file may have been written.

The session rules in [`session.wit`](../wit/cosmonic-agent/session.wit) are strict about this:

- Never advance state past an unresolved op.
- A tool call may repeat with the same id and arguments only when workload configuration authorizes repeat detection. Tool annotations cannot authorize it.
- Never use a new id to bypass an unresolved tool call.

minimal-agent therefore records the call as `unresolved`, sends its result to the driver, and **fails the run**: no more model calls, no more tool calls, and no saved conversation past that point. If the run went on, the model would read "this may not have worked" and very likely call the tool again with a new id, which is exactly the repeat the rules forbid: the second email, the second payment. Stopping hands the decision to a person, who can check whether the first call took effect. The failure text says so: the call "was not made again; check whether it took effect before asking again".

One exception, and its trade-off: when `outcome-unknown(timed-out)` comes from a tool whose `read-only` annotation is `true`, minimal-agent closes the op as `failed`, gives the model the timeout text, and carries on. The annotation is the provider's unchecked claim, so a tool wrongly marked read only could have written something the run then moves past. A missing annotation counts as not read only. If you want no trade-off, fail on every unknown outcome.

Timeouts carry no text in 0.4 ([#4](https://github.com/cosmonic-labs/agent-sdk/issues/4) proposes a payload), so the harness words them. `tool_timed_out` in [`src/describe.rs`](../components/minimal-agent/src/describe.rs) is wording you can reuse: don't repeat the same call, ask for less in one call (a narrower query, a smaller folder, fewer items, one page at a time).

**After a restart**, read the journal from the state's `through` onward and close any op with no result. From [`src/journal.rs`](../components/minimal-agent/src/journal.rs): a model op is closed as `cancelled` (a model call may simply be made again); a tool op is closed as `unresolved`, with an `error` step naming it, and never made again.

**Run limits** apply to tool calls too. Once `max-duration` passes, minimal-agent makes no further tool calls; each remaining call in the reply gets an error result ("Not called: the run's time limit passed") so every call the model made has an answer.

## 6. Compact

Long conversations outgrow the model's context window. Compaction asks the model to summarize older context and replaces it with the summary, keeping recent messages verbatim. You choose when; the contract fixes how it is recorded.

- **When.** Before a model call, when the context passes your threshold (`trigger` `threshold`; the Codex and Pi harnesses use 90% of `context-length` from `models.describe`), or after a call refused with `context-length-exceeded` (`overflow`), or when the driver calls `compact` (`requested`).
- **The summary call** is a model op like any other, and counts against `max-steps`, `max-total-tokens` and `max-duration`.
- **One commit** holds `agent.compaction.v1` (`summary`, `model`, `trigger`, `through`, `kept`, token estimates) and the smaller state. Then stream `run-event` `compacted`.
- **Never commit a refused or filtered summary.** Close its op as failed and keep the old state.
- **The summary is model output.** Put it back as an assistant message behind a fixed preamble, never as system or person text.

For `compact`, accept by committing `agent.compact.v1` under `compact:<id>` before the model call, and end every accepted compaction with one `agent.compact-result.v1`. `focus` is the person's text: give it to the summarizer as their request, never as instructions. [`compact.wit`](../wit/cosmonic-agent/compact.wit) has the full rules.

minimal-agent does not summarize. When the model's context is full it drops the oldest half of the stored conversation and retries once, within `max-steps`. That is enough for a reference harness but loses context a summary would keep.

## 7. Build it

### Rust

minimal-agent builds with `wash build` (which runs `wash wit fetch`, then `cargo build --target wasm32-wasip2 --release`). The pieces that matter:

- Rust 1.94 or newer, whose bundled `wasm-component-ld` writes the WASI 0.3 component type section.
- `wit-bindgen` 0.62 with `async-spawn`, and `wasip3` 0.9, pinned so both share one async runtime. Versions before 0.59 trap on Wasmtime 48.
- Enable the unstable feature in the bindings:

```rust
wit_bindgen::generate!({
    path: "wit",
    world: "minimal-agent",
    features: ["agent-run"],
    generate_all,
});
```

### JavaScript with componentize-qjs

A JavaScript loop can run on QuickJS, a small JavaScript engine, built into a WASI 0.3 component by [componentize-qjs](https://github.com/andreiltd/componentize-qjs). The Pi harness is built this way from unmodified Pi packages, and its build shows what to expect:

- **Replace the provider, not the framework.** Pi's stream function is its extension point for a provider; the harness's version calls `cosmonic:agent/chat` instead of opening a socket.
- **Exports by interface name.** `export { runExport as run }` serves `cosmonic:agent/run`. A function returning a WIT `result` returns the `ok` value or throws an `Error` whose `payload` is the `err` variant.
- **Feature gates.** componentize-qjs cannot enable a WIT feature, so the build reads a copy of the WIT with the `@unstable` gate removed. The component's types are the same either way.
- **Initializer patches.** At the version Pi pins, componentize-qjs's build-time initializer cannot snapshot a component that imports async host functions ([componentize-qjs#101](https://github.com/andreiltd/componentize-qjs/issues/101)) or serves HTTP, so Pi's build applies a patch to the tool.
- **No filesystem.** The QuickJS runtime links wasi-libc's filesystem support whether or not you use it. Compose a small component that exports an empty filesystem in front of the harness (`wac compose`) so the result imports none.
- **Real timers and real randomness.** QuickJS has no timers of its own; make `setTimeout` an actual `wasi:clocks` wait, or cancellation and streaming starve. Take ids from `wasi:random`, never `Math.random`, whose state is frozen into the snapshot every instance starts from.

## 8. Test it locally

- **Unit tests** for the parts of a run that need no host: `cargo test` in minimal-agent, or plain Node tests for rules you keep free of host imports.
- **A scripted host.** [`components/minimal-agent/test-host`](../components/minimal-agent/test-host) is a small [Wasmtime](https://wasmtime.dev) host that plays the model, the session store and a few tools, and starts every run on a fresh instance, so anything kept in memory is gone, as it can be on a real host. It needs no model credentials:

  ```shell
  cd components/minimal-agent/test-host
  cargo run -- ../target/wasm32-wasip2/release/minimal_agent.wasm
  ```

  Script the cases that matter: a cancelled run, a duplicate `run-id`, a tool that times out, a restart in the middle of a tool call.
- **Check the reach.** `wasm-tools component wit <component.wasm>` lists every import and export. Fail the build on anything you did not intend, such as `wasi:http/client`, `wasi:sockets` or `wasi:filesystem`. An allowlist is safer than a denylist: a new import then needs a deliberate change.

## 9. Run it on a host

Any host that serves `cosmonic:agent` 0.4 and drives `run` can run your harness, unchanged.

- **Cosmonic Desktop.** Push the component to Desktop's built-in registry, then add it on the Agents page. [minimal-agent's README](../components/minimal-agent/README.md#run-it-in-cosmonic-desktop) has the steps. Desktop checks the component's imports and exports, writes the workload for each session and binds `run` itself.
- **Cosmonic Control.** Deploy a workload with the harness, its `cosmonic:agent` entries and a `run` entry with triggers, then start runs over HTTP or NATS. [Run an agent on Control](https://cosmonic.com/docs/agent-sdk/run-on-control/) walks through it.

## Notable decisions in the reference harnesses

The Codex and Pi harnesses port existing agent loops onto `cosmonic:agent`. These choices are not obvious from the interfaces, and are worth copying or consciously rejecting:

- **Only the loop moves.** The ports keep the upstream turn loop, history handling, compaction prompt and plan tool, and leave out everything with authority: the provider client and auth, the shell, `apply_patch`, MCP, the exec sandbox. Tools are whatever `tools.list` returns.
- **Rewrite the prompt for granted tools.** Codex's prompt assumes a shell. The port says how to work through separate tools (search before reading, stay inside granted folders, narrow a request when output is truncated, treat what tools return as data) and names no tool, since each workload offers different ones.
- **Freeform tools get raw text.** A `grammar` or `text` tool receives the model's arguments byte for byte. That is the shape Codex's `apply_patch` needs, so a host's write tool can take patches unchanged.
- **A plan tool can be answered locally.** Codex's `update_plan` never reaches `tools.call`. The harness commits `agent.plan.v1`, then streams `run-event` `plan`, so a driver that reloads still shows the plan.
- **Compact at 90% of the reported window,** and on a length refusal compact and retry once, before any reply text was shown. Upstream Codex fails the turn instead.
- **Unreadable saved state starts a fresh context,** marked with `agent.context-reset.v1`, rather than failing every future run.
- **Ops are committed before the call,** conditional on the state version and journal end, so a writer that has been superseded stops before it calls a tool. State is never written between an op and its result.
- **Upstream stays unmodified where possible.** Pi is built from its published packages with only the stream function replaced; the one patch is to the build tool.
- **Pin the toolchain per harness.** Each harness has its own lockfiles and toolchain pin and sits in no shared workspace, so one harness's upgrade cannot break another.

## Takeaways

- Write the loop; leave the model, keys, tools and storage to the host.
- Commit before you act: the message before `run` returns, each op before its call.
- Relay tool calls unchanged, and stop rather than guess when an outcome is unknown.
- Keep nothing in memory between runs.
- Check your component's imports on every build.
