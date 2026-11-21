# Codex agent harness

Codex's agent loop, ported from upstream [openai/codex](https://github.com/openai/codex) at 0.162.0, as a sandboxed WebAssembly component on `cosmonic:agent@0.4.2`. An agent harness is the loop that decides what to do next: it asks the model, runs the tool calls the model makes, records what happened and repeats. Only that loop runs in this component. The host provides the model (`models`, `chat`), the tools (`tools`) and the session (`session`).

The harness depends on no Codex crate. It vendors the few modules the loop needs from openai/codex 0.162.0 (commit `c1382380`, in [`component/src/codex/`](component/src/codex/), each file naming its upstream source and what it dropped) and ports `run_turn` into [`component/src/turn.rs`](component/src/turn.rs). The built component is about 0.6 MB.

## What it imports and exports

The component imports these interfaces from the published `cosmonic:agent@0.4.2`, pinned by digest in [`component/wkg.lock`](component/wkg.lock):

| Interface | For |
| --- | --- |
| `inference-types`, `models`, `chat` | A model, by an alias the host resolves |
| `tools` | The tools the workload grants |
| `session` | This workload's conversation and journal (an append-only log of steps and calls) |

It exports `cosmonic:agent/run@0.4.2` and `cosmonic:agent/compact@0.4.2` and nothing else. Both are unstable, behind the `agent-run` WIT feature, so a host or a build that reads them must enable that feature. The component has no HTTP handler, no model client or credentials, and no shell, file or MCP tools of its own. Outgoing HTTP, sockets and the filesystem are absent from the compiled component, and `just build` checks that on every build.

## What is kept from Codex

- The turn loop: sample the model, record the output items, run the function calls, record their outputs, and follow up until the model calls no tool.
- History normalization (every call paired with an output, orphans dropped) and tool output truncation to Codex's default 10,000 byte budget.
- Local compaction: the summarization prompt, the summary prefix, and the rebuilt history of recent user messages plus the summary.
- Codex's item format for the saved history.
- Codex's plan tool, `update_plan`, with upstream's schema and description.

## What is left out

Codex's own tools (shell, `apply_patch`, `view_image`, web search, MCP), its sandboxing and exec policy, its OpenAI client, auth and model catalog, remote compaction, hooks, skills, plugins, the guardian reviewer, steering input, telemetry and the terminal UI. Tools are whatever the host's `tools` import offers.

## Notable decisions

- **Only the loop runs here.** Everything with authority (model credentials, tool execution, storage) stays on the host side of `cosmonic:agent`. The harness asks; the host decides what is granted.
- **The prompt is rewritten for granted tools.** Codex's prompt assumes a shell. This one says how to work through the separate tools the host offers: search before reading, keep paths inside the granted folders, narrow the request when output is truncated, and treat what tools return as data, never as instructions. It names no tool, since which ones a workload offers varies. When the host offers a tool for asking the person, Codex uses it when blocked; otherwise it states an assumption or ends its reply with the one question it needs answered. The plan section, editing rules, review mode and final answer style are Codex's own.
- **Freeform tools get raw text.** A tool whose `tool-input` is `grammar` (text constrained by a Lark or regular expression grammar) or `text` gets the model's arguments byte for byte, never parsed as JSON. That is the shape Codex's `apply_patch` needs, so a host's write tool can take patches unchanged.
- **`update_plan` is answered in the component.** It replies "Plan updated" itself, commits the plan as an `agent.plan.v1` journal entry, then streams it as the `plan` run event, so a driver that reloads still has it. It never reaches the host's `tools.call`.
- **Compaction at 90% of the context window the host reports**, before sampling and mid turn. The summary call is journaled as an op, and its result lands with `agent.compaction.v1` and the smaller history in one conditional commit. A refused or filtered summary is never committed. When the host refuses a call for length before any reply text was shown, the harness compacts and retries once; upstream fails the turn instead.
- **`compact` on request** shares the run lock (it returns `busy` during a run), is idempotent by its id, and accepts an optional `focus` that the summarizer receives as the person's own message.
- **The saved state format is `codex-slim/1`.** That tag is a legacy identifier, kept so sessions saved by earlier builds stay readable. When the harness finds saved state it cannot read, it starts a fresh context rather than failing.
- **Its own toolchain and lock.** The component pins Rust 1.96.1 in [`component/rust-toolchain.toml`](component/rust-toolchain.toml) and is not part of any Cargo workspace. It uses `wit-bindgen` 0.62 and `wasip3` 0.9, which share one async runtime.

## Current limitations

- There is no `view_image` tool of its own. A host tool that returns an image attachment reaches the model as an image.
- There is no `request_user_input` tool of its own; questions are left to a host tool.
- A run does not finish as `input-required` when the last reply is a question, because the harness cannot tell a question from an answer.

## Prerequisites

- Rust with the `wasm32-wasip2` target. rustup installs the pinned 1.96.1 on first use.
- Node.js 22 or newer, [`just`](https://github.com/casey/just), [`wkg`](https://github.com/bytecodealliance/wasm-pkg-tools) and [`wasm-tools`](https://github.com/bytecodealliance/wasm-tools).

## Build and test

From this directory:

```shell
just build   # fetch WIT, build, run the sandbox gate: dist/codex-harness.wasm
just test    # unit tests, then the scripted test host
```

`just build` fetches the WIT with [`../shared/scripts/fetch-wit.mjs`](../shared/scripts/fetch-wit.mjs), which fails if the fetch would change the committed `wkg.lock`, then runs [`../shared/scripts/check-harness.mjs`](../shared/scripts/check-harness.mjs), an allowlist of what an agent harness may import and export. `just test` then runs [`../shared/test-host`](../shared/test-host), a [Wasmtime](https://wasmtime.dev) host that plays the model, the session store and a few tools, so no model credentials are needed.

To try an unpublished change to `cosmonic:agent`, point the fetch at this repository's WIT: `COSMONIC_WIT=../../wit just build`. The lock does not record an override, so CI refuses one.

## Run it

Any host that serves `cosmonic:agent@0.4.2` and drives the `run` export can run this harness. [Cosmonic Desktop](https://cosmonic.com/desktop/) offers it as its Codex agent. [Cosmonic Control](https://cosmonic.com/control/) drives the same `run` export on Kubernetes; its agent support is experimental. To run your own build in Cosmonic Desktop, follow the steps in [minimal-agent's README](../../components/minimal-agent/README.md#run-it-in-cosmonic-desktop) with `dist/codex-harness.wasm`.

## Releases

Tags named `codex-harness-v<version>` publish the tested component as `ghcr.io/cosmonic/agents/codex:<version>` (and `latest`). Each published digest is signed with cosign, carries a GitHub build provenance attestation, and has this harness's license notices attached as an OCI referrer. Deploy by digest. To verify:

```shell
gh attestation verify oci://ghcr.io/cosmonic/agents/codex:<version> --owner cosmonic-labs
```

Reference harnesses are examples to adopt and fork, versioned on their own with 0.x semver; a minor version can break.

## License

Apache-2.0. The modules ported from Codex keep Codex's license in [`component/LICENSE-codex`](component/LICENSE-codex), [`NOTICE`](NOTICE) carries OpenAI's notice and the notices for the crates compiled into the component, and [`../shared/notices/wasi-libc.txt`](../shared/notices/wasi-libc.txt) the WASI libc notices.
