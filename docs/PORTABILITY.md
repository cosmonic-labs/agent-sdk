# Portability: what every host shares, and what is a host's own

An agent harness built on `cosmonic:agent` runs on any host that serves the interfaces it imports and drives the export it serves. A host is the runtime that runs your component, makes model calls for it, keeps its session and runs its tools; [Cosmonic Desktop](https://cosmonic.com/desktop/) and [Cosmonic Control](https://cosmonic.com/control/) are two. This page separates the contract, which is the same on every host, from the conventions a host adds around it, so you know what to rely on and what to look up per host.

The short version: your component's imports and exports are the contract. How runs get started, what a workload manifest says, and which keys configure the host are conventions.

| Portable: the same on every host | Host convention: varies by host |
| --- | --- |
| The `cosmonic:agent` packages, interfaces and worlds | How a run is triggered (chat window, HTTP, messaging) |
| What `run` and `compact` mean, and the events and outcomes they produce | Request formats, headers, labels and subjects |
| Tool calls, tool errors and tool input kinds | Which components a session offers as tools, and the names the model sees |
| The session journal, its conditional commits and its reserved entry kinds | Where the session is stored and how it is encrypted |
| The capability rule: a component reaches only what it imports and is granted | The manifest keys that grant those interfaces |
| Versioning and compatibility rules | Default limits, deadlines and caps |

## The portable contract

### Packages and worlds

`cosmonic:agent` is a WIT package (WebAssembly Interface Types, the interface language components are built against). The current release is `cosmonic:agent@0.4.2`, defined in [`wit/cosmonic-agent`](../wit/cosmonic-agent) and published to `ghcr.io/cosmonic-labs/cosmonic/agent`. A world is the full list of what a component imports and exports. The ones a harness author uses are in [`world.wit`](../wit/cosmonic-agent/world.wit):

| World | Stability | Imports | Exports | For |
| --- | --- | --- | --- | --- |
| `agent` | stable | `inference-types`, `models`, `chat`, `tools`, `session` | nothing | What a harness may use |
| `runnable-agent` | unstable (`agent-run`) | everything in `agent` | `run` | A harness a host drives |
| `compactable-agent` | unstable (`agent-run`) | everything in `agent` | `run`, `compact` | A harness that also summarizes on request |
| `run-client` | unstable (`agent-run`) | `run` | nothing | A driver, protocol adapter or test double |
| `compact-client` | unstable (`agent-run`) | `run`, `compact` | nothing | A driver for a `compactable-agent` |
| `tool-provider` | stable | nothing | `tools` | A tool that lists its own definitions and parses JSON itself |

The package also defines provider worlds (`inference-provider`, `inference-client`, `chat-provider`, `embeddings-provider`) for trusted model-serving components. A harness never targets them.

### A run

`run` ([`run.wit`](../wit/cosmonic-agent/run.wit), types in [`run-types.wit`](../wit/cosmonic-agent/run-types.wit)) is one unit of work: the person's message in, the agent's loop of model and tool calls, a reply out. Chat interfaces call it a turn.

```wit
run: async func(
    message: user-message,
    options: run-options,
) -> result<started-run, start-error>;
```

These rules hold on every host:

- **Acceptance.** `run` returns once the harness has committed the person's message and an `agent.run.v1` entry to the session, under the commit id `run:<run-id>`. A refusal (`start-error`: `busy`, `duplicate`, `invalid-input`, `unavailable`, `other`) writes nothing.
- **Progress, then outcome.** `started-run` is a stream of `run-event`s (`text-delta`, `reasoning-delta`, `step`, `counts`, `compacted`, `plan`, `other`) and a future that resolves to a `run-outcome` or a `run-error`. The stream closes before the future resolves.
- **Cancellation.** Dropping the event stream asks the harness to stop. It ends its calls, records them, and resolves `cancelled` once nothing of the run is still running.
- **One run at a time per session.** The driver serializes runs. The harness's `busy` answer is a backstop.
- **The host chooses the session.** `run` takes no session handle, so a driver cannot point a run at someone else's conversation.
- **Limits.** `max-steps`, `max-total-tokens` and `max-duration` in `run-options` are enforced by the harness, so it can end with a reply (`finish`: `max-steps`, `max-total-tokens`, `max-duration`). A driver also enforces what it can meter itself.
- **No in-memory state between runs.** The next run may land on a fresh instance. Everything needed to continue is in the session.
- **Approvals are the host's.** A harness never receives or answers an approval. A held tool call simply waits, then returns a result or `not-dispatched(denied)`. When nobody can answer a question, the run ends with `finish` `input-required`.

`compact` ([`compact.wit`](../wit/cosmonic-agent/compact.wit)) follows the same lock and cancellation rules. It summarizes older context between runs, at the driver's request.

### Tool calls

`tools` ([`tools.wit`](../wit/cosmonic-agent/tools.wit), records in [`tool-types.wit`](../wit/cosmonic-agent/tool-types.wit)) has two functions: `list`, which returns `tool-def`s, and `call`, which takes the model's `tool-call` as it came and returns a `tool-response`. The same interface is imported by a harness and exported by a tool provider.

- A tool's input is a `tool-input`: `json-schema`, `grammar` (text a Lark or regex grammar constrains) or `text`. A call's arguments are a `tool-arguments`: `json` for a JSON Schema tool, `text` for the other two, never parsed as JSON.
- A completed failure is `tool-response.is-error`, returned to the model as a normal result.
- `call-error` says what is known. `not-dispatched` means this attempt had no effect. `outcome-unknown` means it may have.
- `list` fails with `list-error` rather than returning an empty list, so "no tools" and "couldn't list" are different answers.
- Output may be truncated by the host, which appends a note with the original size.
- `tool-annotations` (`read-only`, `destructive`, `idempotent`, `open-world`) are the provider's claims for display. They authorize nothing.
- `find_tools` and `load_tools` are reserved tool names for a host's own discovery tools.

A **typed tool** exports its own interface and nothing from `cosmonic:agent`. The host turns its functions into `tool-def`s and translates JSON to typed calls. The harness never sees the difference: it receives `tool-def`s and relays `tool-call`s either way. How a host builds those definitions (names in snake case, record fields as arguments, doc comments as descriptions) is described in the [design note](../wit/cosmonic-agent/README.md#what-makes-a-component-a-tool). It is written as how hosts translate, not as WIT types, and a host may present a different name to the model, so a harness must not hard-code tool names it did not list.

### The session journal

`session` ([`session.wit`](../wit/cosmonic-agent/session.wit)) is an append-only journal plus opaque working state that the host keeps for the harness. A commit appends entries and optionally replaces state atomically, on conditions (`require-state`, `require-end`). A commit whose conditions no longer hold writes nothing and returns the current snapshot, so a writer never overwrites another; reconciling is the writer's job, or it stops (minimal-agent ends the run `superseded`). A `commit-id` makes a retried commit safe.

All `agent.*` entry kinds are reserved, and each entry's data is JSON. A host or a person's tool can redraw a conversation from these alone:

| Kind | Holds |
| --- | --- |
| `agent.step.v1` | A message: `role` (user, assistant, tool, error), `text`, `blocks`, `tools`, and `run` when a run wrote it |
| `agent.op.v1` | A model or tool call before it starts: `op`, `target`, `name`, tool `arguments` |
| `agent.op-result.v1` | The call's end: `op` and `outcome` (`succeeded`, `failed`, `cancelled`, `unresolved`) |
| `agent.run.v1` / `agent.run-result.v1` | A run's opening and its ending (`outcome` with `finish` or the error case) |
| `agent.compact.v1` / `agent.compact-result.v1` | A requested compaction's opening and its ending |
| `agent.compaction.v1` | A summary that replaced older context: `summary`, `model`, `trigger`, `through`, `kept`, token estimates |
| `agent.plan.v1` | The agent's whole current plan, replacing any earlier one |
| `agent.context-reset.v1` | Saved state could not be read, so the harness started a fresh context |

Writers may define other kinds. Readers skip kinds and fields they do not understand.

### Compaction

Compaction replaces older context with a model-written summary so the next model call fits. The contract fixes how it is recorded, not when the harness does it:

- `trigger` says why: `threshold` (the harness's own limit), `overflow` (a model call was refused for length) or `requested` (the driver called `compact`).
- The summary call is an op like any model call, and `agent.compaction.v1` lands in one commit with the smaller state.
- A refused or filtered summary is never committed.
- Earlier entries stay in the journal, and the summary is model output: never system or person text.

### Capabilities

A component reaches only what its world imports and its host grants. A harness on the `agent` world imports a model by alias, its tools and its session, and nothing else: no endpoint, no key, no network, no files. Exporting `run` adds no power, because an export is something others call, not something the component can use. You can check a built component's reach on any machine:

```shell
wasm-tools component wit target/wasm32-wasip2/release/minimal_agent.wasm
```

Some language runtimes bring extra imports. Rust's standard library adds `wasi:cli`, `wasi:io` and `wasi:clocks` 0.2 plumbing to print a panic message; none reaches the network or the filesystem. A host may refuse a harness that imports more than this, and the reach a host grants each import is its own decision.

### Versioning and compatibility

From the [design note](../wit/cosmonic-agent/README.md#versioning):

- **Breaking changes bump the minor version** (0.4 to 0.5). Adding a case to a variant, a field to a record, or a function to an interface a guest exports (`tools`, `run`) breaks compiled consumers.
- **Additive changes bump the patch version** (0.4.0 to 0.4.1) and carry `@since` on the new items. New interfaces, and new functions on interfaces the host serves, are additive.
- **Patch versions link to each other.** A harness built against 0.4.2 links to a host entry that names 0.4.0 or 0.4.1, as long as the host serves every interface it imports, including the unstable ones.
- **Published versions never change.** 0.4.2 corrected 0.4.0's docs without changing its types.
- **Unstable items may still change.** Items behind `@unstable(feature = ...)` are outside these promises. In 0.4.2 the one feature is `agent-run`, which gates `run`, `run-types`, `compact` and the worlds that use them. A consumer sees these only when it enables the feature (in Rust, `features: ["agent-run"]` in `wit_bindgen::generate!`).

Two things follow for a harness. Handle every case of every variant, including the `other` cases where a variant defines one, and never parse their detail strings. Pin the version you build against, and expect to rebuild for a minor version.

Tool discovery needs no new interface today: a host that offers it adds its own `find_tools` and `load_tools` tools to `tools.list`, and the harness calls them like any other tool. Interfaces for spawning child agents are in design and not part of any release.

## Host conventions

Everything below is a host's choice. Two hosts serving the same `cosmonic:agent` version can differ here and both be correct.

### How runs start

The contract says a driver calls `run`. Who that driver is, and what reaches it, is up to the host:

- **A chat interface.** Cosmonic Desktop drives `run` from its Agents page. It adds the binding to the agent's workload itself and refuses a manifest that names `run` ([minimal-agent's README](../components/minimal-agent/README.md#run-it-in-cosmonic-desktop)).
- **HTTP.** A host can accept requests in some public format and translate them into `run` calls. Cosmonic Control accepts requests in the OpenAI Responses API format at `/v1/responses`, streams server-sent events, and maps `input` onto the `user-message` and fields such as `instructions` and `reasoning.effort` onto `run-options` ([Run an agent on Control](https://cosmonic.com/docs/agent-sdk/run-on-control/)).
- **Messaging.** Control's agent support is experimental. It also starts runs from NATS requests on a per-workload subject, with the same JSON body.
- **An adapter component.** A component on the `run-client` world can import `run` and serve another protocol itself.

The response format, status codes (Control answers a second concurrent request with `409 busy`), cancellation endpoints, authentication and default deadlines all belong to the host. None of them changes what your harness sees: one `run` call with a `user-message` and `run-options`.

### Tools and their names

Which components a session offers as tools is the host's decision. A host may present a different name to the model than the function name, cap output, set a per-call deadline (Control documents 30 seconds) and add its own tools, such as a question for the person or `find_tools` and `load_tools`. Read the list every run, and treat names as the host gave them.

### Sessions

Where the journal is stored, how it is encrypted and when it is deleted are host decisions. Whether a host binds one session per workload or one per conversation is too. Limits such as entry size, session size and the length of a `commit-id` are the host's; a harness learns of them through `session.error.too-large` or a refused commit. A harness that keeps its run ids short leaves room for the `run:` prefix.

### Labels, headers and metadata

`request-meta` in `chat-options` carries `trace-parent`, `labels` and cache hints a host may use or ignore. Host-side labels, such as the node label Control uses to place agent workloads, never reach the component.

## One manifest, many hosts

A wasmCloud workload manifest (`runtime.wasmcloud.dev/v1alpha1` `Workload` or `WorkloadDeployment`) says which components run together and what each is granted. For an agent it lists the harness component, any tool components, and one `hostInterfaces` entry per group of `cosmonic:agent` interfaces, each granted to named components. [`components/minimal-agent/manifests/workload.yaml`](../components/minimal-agent/manifests/workload.yaml) is the shape:

```yaml
hostInterfaces:
  - namespace: cosmonic
    package: agent
    version: 0.4.0
    interfaces:
      - session
    config:
      components: agent
components:
  - name: agent
    image: ghcr.io/cosmonic-labs/components/minimal-agent:0.3.2
    localResources:
      allowedHosts: []
```

What carries from host to host:

- The components and their images. Pin images by digest in production.
- Each component's own grants: `allowedHosts`, volumes and the like. A harness has `allowedHosts: []`.
- One entry per interface group (`session`; `inference-types`, `models`, `chat`; `tools`), each naming the components it is granted to with `components`.
- An entry naming an earlier 0.4 patch version (Desktop writes 0.4.0) links to a harness built against 0.4.2.

What does not carry, and why:

| Concern | Varies because |
| --- | --- |
| The `run` entry | Desktop writes it and refuses one you supply. Control requires it, with `triggers`. |
| Entry `name`s | A host may require distinct names when several entries share the package. |
| Model selection keys | Each host names its model sources its own way (`backend`, `source`, `model`). |
| Tool keys | Which tools a session has, and how they are selected, is host policy. |
| Placement | Node selectors and labels are cluster concerns. |

Each host documents its keys: [Agent workload reference](https://cosmonic.com/docs/agent-sdk/workload/) covers Desktop and Control. The harness image itself never changes between hosts; only the manifest around it does.
