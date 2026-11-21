# Pi agent harness

[Pi](https://github.com/earendil-works/pi)'s agent loop (`@earendil-works/pi-agent-core` 1.5.0) as a sandboxed WebAssembly component on `cosmonic:agent@0.4.2`. An agent harness is the loop that decides what to do next: it asks the model, runs the tool calls the model makes, records what happened and repeats. Pi is built from unmodified upstream packages from npm. It runs on QuickJS (a small JavaScript engine that [componentize-qjs](https://github.com/andreiltd/componentize-qjs) builds into the component) and targets WASI 0.3. The model, the tools and the conversation history come from the host; the component carries no provider SDK, no API key and no endpoint to dial.

## What it imports and exports

The component imports these interfaces from the published `cosmonic:agent@0.4.2`, pinned by digest in [`component/wkg.lock`](component/wkg.lock):

| Interface | For |
| --- | --- |
| `inference-types`, `models`, `chat` | A model, by an alias the host resolves. Pi's stream function is replaced by one that calls the host ([`component/src/llm.js`](component/src/llm.js)), so reply text streams as it is generated and dropping the stream cancels generation. |
| `tools` | The tools the workload grants. The host runs them; the harness never links to a tool directly. |
| `session` | The journal (an append-only log of steps and calls) and the transcript ([`component/src/session.js`](component/src/session.js)) |

It also imports the WASI 0.3 monotonic clock, for timers and the run's deadline, and `wasi:random`, for commit and run ids. Outgoing HTTP, sockets and the filesystem are absent from the compiled component, and `just build` checks that on every build.

It exports `cosmonic:agent/run@0.4.2` and `cosmonic:agent/compact@0.4.2`, both unstable behind the `agent-run` WIT feature, and `wasi:http/handler@0.3.0`, which serves `POST /task` (NDJSON `step`, `delta`, `done` and `failed` lines) and `GET` or `DELETE /history` for hosts that do not call `run`. Both paths run the same loop and write the same journal entries.

## The session

Every write is one `commit` of journal entries and, optionally, the transcript as state:

| When | Entries | State |
| --- | --- | --- |
| a turn starts | `agent.op-result.v1` and an `error` step for each tool op a previous run left unresolved, then the person's `agent.step.v1` and `agent.run.v1`, under the commit id `run:<run-id>` | the transcript including the message |
| a reply asks for tools | the reply's step, then an `agent.op.v1` per call | none, so no state is ever between an op and its result |
| a tool result arrives | its `agent.op-result.v1` and step | the transcript, once no op is open |
| any other reply | its step | the transcript |
| the run ends | an `error` step if it failed or was cancelled, then `agent.run-result.v1` | the transcript as far as it got |

A saved transcript in a format this build cannot read starts a fresh context, marked with `agent.context-reset.v1`.

## Context compaction

Each run asks the host for the model's context window (`models.describe`). When the transcript reaches 90% of it, or the host refuses a call with `context-length-exceeded` before any reply text was streamed, Pi summarizes the messages before the person's latest one through the same model alias and carries on from the summary ([`component/src/compaction.js`](component/src/compaction.js), applied through pi-agent-core's `transformContext`). The summarizing call is journaled as an `agent.op.v1` model op, and its result lands with `agent.compaction.v1` and the smaller transcript in one commit, never with a tool call open. The summary goes back in as an assistant message behind a fixed preamble, never as system text, and every original message stays in the journal. A summary the model refuses, or a provider filter withholds, is never committed. Summaries count against `max-steps` and `max-total-tokens` like any other model call.

The `compact` export lets a driver ask for a summary between runs, with an optional `focus` the summarizer receives as the person's own message. It shares the run lock (`busy` during a run), is idempotent by its id, and keeps the latest exchange verbatim, as the automatic compaction does.

## Notable decisions

- **Pi is unmodified.** The harness bundles Pi's published npm packages as they are and replaces only the stream function, which is Pi's own extension point for a provider. Nothing in Pi is forked or patched.
- **The build tool carries a patch, not Pi.** componentize-qjs needs two fixes to its build-time initializer (Wizer), in [`patches/componentize-qjs.patch`](patches/componentize-qjs.patch), against [andreiltd/componentize-qjs](https://github.com/andreiltd/componentize-qjs) at `eee19a60b144321b2535a50cc58e05b73d9f5242` (v0.4.5 plus one commit):
  - **HTTP.** Upstream stubs `wasi:http` with traps during initialization. The stub mints its own stream resources and fails to link against the real `wasi:io`, which breaks a component that serves HTTP. The patch links the real `wasi:http` interfaces instead. No upstream issue tracks this yet.
  - **Async imports.** Upstream stubs unknown imports as sync functions, which fail the type check for an `async` import ("type mismatch with async"), so a component that imports any WASI 0.3 style host interface (all of `cosmonic:agent`) cannot be initialized. The patch stubs them as concurrent functions that trap. Upstream tracks this in [componentize-qjs#101](https://github.com/andreiltd/componentize-qjs/issues/101).

  Upstream [componentize-qjs#91](https://github.com/andreiltd/componentize-qjs/pull/91) (async import cancellation and larger snapshots) and [#90](https://github.com/andreiltd/componentize-qjs/pull/90) (async export task ownership) have merged since the pinned commit, so a toolchain bump may shrink the patch. Re-test before bumping.
- **deny-fs answers the filesystem inside the component.** The QuickJS runtime is built with wasi-libc, which links its filesystem support whether or not the harness touches a file, so the bare QuickJS component imports `wasi:filesystem`. [`deny-fs/`](deny-fs/) is a small Rust component that exports an empty filesystem: no preopened directories, and every operation refused with `access`. Composed in front of the harness ([`component/compose.wac`](component/compose.wac)), it satisfies those imports inside the component, so the published artifact imports no filesystem at all. Stubs that trap would crash the harness on a probe; an empty filesystem refuses instead. The interface version in [`deny-fs/wit/world.wit`](deny-fs/wit/world.wit) (`@0.2.12`) has to match what the QuickJS runtime imports; if a componentize-qjs update moves it, `wac compose` fails naming the import.
- **Timers really suspend.** A microtask never returns control to the host; only a real suspension does. [`component/src/timers.js`](component/src/timers.js) makes `setTimeout` an actual `wait-for` of at least 1 ms, without which cancellation and response streaming starve.
- **Ids come from `wasi:random`, never `Math.random`.** The PRNG state is part of the Wizer heap snapshot every instance starts from, so `Math.random` would repeat across instances.
- **Ops are committed before the call.** A reply's tool ops are committed conditionally on the state version and the journal end, so a writer that has been replaced stops before calling the tool, and unresolved tool calls are never repeated.

## Known issue

With the pinned componentize-qjs, the scripted test host's `compact` export cases time out (`just test-e2e`); the run, duplicate and cancellation cases pass. It is not yet known whether this is the toolchain or the harness. A componentize-qjs bump past #90 and #91 is the first thing to try. Until it is resolved, `just test` runs the build, the sandbox gate and the rule tests, and CI runs the scripted host without blocking on it.

## Notes on QuickJS, for anyone editing `component/src/`

- A `u64` is a JS number here (jco and StarlingMonkey use `BigInt`).
- A WIT function returning `result` returns the `ok` value or throws an `Error` whose `payload` is the `err` variant (`{tag, val}`); results inside streams and futures are plain `{tag, val}` objects.
- Stream and future payload types are looked up by the constant names the shim generates (`wit.Stream.MESSAGE`, `wit.Future.RESULT_VOID_STRING`, ...), which `component/src/llm.js` and `component/src/entry.js` do by shape, so a renamed constant fails with the list of names that exist.
- The QuickJS build also exports its initializer (`init`); `component/compose.wac` does not re-export it.

## Prerequisites

- Node.js 22 or newer and pnpm 12 (`corepack enable` picks the version `package.json` names).
- Rust with the `wasm32-wasip2` target, for `deny-fs`.
- [`just`](https://github.com/casey/just), [`wkg`](https://github.com/bytecodealliance/wasm-pkg-tools), [`wasm-tools`](https://github.com/bytecodealliance/wasm-tools) and [`wac`](https://github.com/bytecodealliance/wac).
- The patched componentize-qjs. To build it:

  ```shell
  git clone https://github.com/andreiltd/componentize-qjs
  cd componentize-qjs
  git checkout eee19a60b144321b2535a50cc58e05b73d9f5242
  git apply path/to/harnesses/pi/patches/componentize-qjs.patch
  cargo build --locked --release -p componentize-qjs-cli
  export COMPONENTIZE_QJS=$PWD/target/release/componentize-qjs
  ```

  The patch carries the `Cargo.lock` lines for the one dependency it adds, so the CLI builds `--locked`. On a first build, componentize-qjs's build script downloads wasi-sdk 33 and binaryen 130 without checking a digest; to pin those, point `WASI_SDK_PATH` and `WASM_OPT` at copies you have verified, as CI does.

## Build and test

From this directory, with `COMPONENTIZE_QJS` set:

```shell
just build       # bundle, componentize, compose deny-fs, run the sandbox gate: dist/pi-harness.wasm
just test        # build, then the rule tests under Node
just test-unit   # the rule tests alone (Node only)
just test-e2e    # the scripted test host (see Known issue)
```

The build is locked: `pnpm install --frozen-lockfile` uses [`pnpm-lock.yaml`](pnpm-lock.yaml), `deny-fs` builds `--locked`, and the WIT is pinned by `component/wkg.lock` and `deny-fs/wkg.lock`. The harness's journal, run and compaction rules that do not depend on Pi live in [`../shared/js`](../shared/js) so they run under Node's tests.

## Run it

Any host that serves `cosmonic:agent@0.4.2` and drives the `run` export can run this harness. [Cosmonic Desktop](https://cosmonic.com/desktop/) offers it as its Pi agent. [Cosmonic Control](https://cosmonic.com/control/) drives the same `run` export on Kubernetes; its agent support is experimental. To run your own build in Cosmonic Desktop, follow the steps in [minimal-agent's README](../../components/minimal-agent/README.md#run-it-in-cosmonic-desktop) with `dist/pi-harness.wasm`.

## Releases

Tags named `pi-harness-v<version>` publish the tested component as `ghcr.io/cosmonic/agents/pi:<version>` (and `latest`). Each published digest is signed with cosign, carries a GitHub build provenance attestation, and has this harness's license notices attached as an OCI referrer. Deploy by digest. To verify:

```shell
gh attestation verify oci://ghcr.io/cosmonic/agents/pi:<version> --owner cosmonic-labs
```

Reference harnesses are examples to adopt and fork, versioned on their own with 0.x semver; a minor version can break.

## License

Apache-2.0. Pi, QuickJS-ng and the other code compiled into the component keep their own licenses; [`NOTICE`](NOTICE) carries their notices, and [`../shared/notices/wasi-libc.txt`](../shared/notices/wasi-libc.txt) the WASI libc notices.
