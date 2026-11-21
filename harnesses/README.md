# Reference agent harnesses

An agent harness is the loop that decides what to do next: it asks the model, runs the tool calls the model makes, records what happened and repeats. The harnesses here take that loop from an existing agent and run it as a WebAssembly component on `cosmonic:agent`. The host makes the model calls, runs the tools and keeps the session, so a harness gets only what a workload grants it.

They are examples to adopt and fork. Each one is versioned on its own with 0.x semver, so a minor version can break.

| Path | What it is | Published as |
| --- | --- | --- |
| [`codex`](codex) | Codex's agent loop, ported from openai/codex 0.162.0 (Rust) | `ghcr.io/cosmonic/agents/codex` |

## Shared pieces

- [`shared/scripts/fetch-wit.mjs`](shared/scripts/fetch-wit.mjs) fetches a harness's WIT through [`../wit/wkg-registries.toml`](../wit/wkg-registries.toml) and fails if the fetch would change its committed `wkg.lock`.
- [`shared/scripts/check-harness.mjs`](shared/scripts/check-harness.mjs) is the sandbox gate. It holds a built harness to an allowlist: `cosmonic:agent` interfaces at one version, WASI clocks and random, the io and cli plumbing a language runtime links, and `wasi:http/types` (which alone cannot send a request). Outgoing HTTP, sockets and the filesystem fail the build.
- [`shared/test-host`](shared/test-host) is a scripted [Wasmtime](https://wasmtime.dev) host. It plays the model, the session store and a few tools, starts every call on a fresh instance, and runs a shared contract suite plus each harness's own cases. No model credentials are needed.

Each harness's README covers what it imports, what it keeps from upstream, its notable decisions, and how to build, test and run it.
