# cosmonic:agent

The interfaces an agent component uses to call models, call tools and keep a
durable conversation, and the export a host uses to hand it work. This page is
the design note for the package: how the pieces fit, and what a host, an agent
or a tool author can rely on. The `.wit` files are the contract; the
[repository README](../../README.md) lists the worlds, and
[docs/migrating-to-0.4.md](docs/migrating-to-0.4.md) lists what changed from
0.3.0.

## Two sides of an agent

An agent has two interfaces, and they stay separate.

- **What it uses** (the `agent` world): `models` to get a model by a
  workload-scoped alias, `chat` to call it, `tools` to list and call tools,
  and `session` to read and append its conversation. The host serves all four
  and decides what each reaches. An agent never names an endpoint or holds a
  credential.
- **How it is driven** (`runnable-agent`, unstable feature `agent-run`): the
  agent exports `run`, and whatever drives it (a chat UI, an HTTP ingress, an
  event trigger, a parent agent) calls that. `compactable-agent` adds
  `compact`, so a driver can ask for a summary between runs.

Exporting `run` adds no power: the agent reaches exactly what its imports
allow.

## Running an agent

`run(message, options)` is one unit of work: the person's message in, the
agent's loop of model and tool calls, a reply out.

- **Acceptance.** `run` returns once the agent has committed the message (and
  `agent.run.v1`) to the session, so a run that fails later still shows what
  was asked. A refusal (`start-error`) writes nothing.
- **Progress.** A stream of `run-event`s: text deltas, whole steps, the
  agent's current `plan` (each event replaces the last) and `compacted` when it
  summarizes its context. Then a future resolves to a `run-outcome` or a
  `run-error`.
- **Cancellation.** Dropping the event stream asks the agent to stop. It ends
  its model call and pending tool calls, records them, and resolves
  `cancelled` once nothing of the run is still running. A driver that wants
  only the outcome still reads and discards events.
- **One run at a time per session.** The driver enforces it. The agent's own
  `busy` check is a backstop against two drivers on one session.
- **Limits.** `max-steps`, `max-total-tokens` and `max-duration` (counted from
  acceptance) are enforced by the agent so it can end with a reply. A driver
  also enforces what it can meter itself, such as a deadline and byte caps.
- **The journal.** `agent.run.v1`, `agent.run-result.v1`, `agent.plan.v1`,
  `agent.compaction.v1` and the compact entries are reserved in `session`, so
  a host can redraw a conversation from the journal alone.

The host chooses the session: `run` takes no session handle, and the session
stays an import the host binds. Approvals stay at the host's own gates, so an
agent can't fake one. When nobody is there to answer, a run ends
`input-required` and the answer starts the next run.

## Tools

### What an agent sees

`tools.list` returns `tool-def`s: a name, a description, and an `input`:
a JSON Schema for the arguments (`json-schema`), text a grammar constrains
(`grammar`), or plain `text`. `tools.call` takes the model's
`tool-call` as it came, with `json` or `text` arguments, and returns a
`tool-response` with text and attachments, optional structured output, and
`is-error` for a call that ran and failed.

The agent relays what the model produced, JSON or text, so this boundary is
untyped. The agent doesn't need to know any tool's types at build time. The
host checks JSON arguments against the tool's schema; a grammar steers the
model but is not checked, so a tool validates its own text input.

Errors say what is known. `not-dispatched` means this attempt had no effect;
`outcome-unknown` means it may have. `tools.list` fails with `list-error`
rather than returning an empty list. A host may cap a tool's output and note
the original size.

### What makes a component a tool

A component becomes a tool in one of two ways.

1. **A tool provider** exports `cosmonic:agent/tools` (the `tool-provider`
   world). It lists its own definitions and parses JSON itself. An MCP bridge
   in front of an MCP server works this way.
2. **A typed tool** exports its own interface and nothing from
   `cosmonic:agent`, for example a `github` package with `pulls` and `issues`
   interfaces. A host that serves `tools` offers its functions to the agent and
   translates between JSON and the WIT types. The same component can be
   imported directly by code that wants the real types, with no JSON at all.

A typed tool is not marked as a tool in its WIT. Which components a session
offers as tools is the host's decision, from its catalog, so any component
with a suitable interface can serve, including one published by someone else.

Cosmonic hosts translate typed tools this way:

- Every function of each exported interface outside `wasi:*` and
  `cosmonic:agent/*` becomes a tool. Its name is the function name in snake
  case (a host may present a different name to the model), and its
  description is the function's doc comment.
- A function whose only parameter is a record takes the record's fields as its
  arguments, so each field's doc comment describes one argument. Write the
  default an `option` field takes into its comment.
- JSON property names are the WIT names in snake case. `option<T>` may be
  omitted or `null`; a `bool` may be omitted and means `false`. A variant is
  `"case"`, or `{"case": payload}` with a payload; flags are a list of names.
- A `result`'s `err` is an error the model reads, not a failed call. A result
  record with a `text` field is shown as that text, with its other fields as a
  short JSON header. A field named `image`, a record of `media-type` and
  `data`, is passed to the model as an image.
- A function whose single parameter is a `string` and whose doc comment has a
  paragraph starting `Freeform:` is offered with `text` input.
- Functions that take or return resources, streams or futures have no JSON form
  and are not offered.
- Descriptions come from the tool's published WIT package, because a built
  component keeps types but not doc comments. The package's standard binary
  encoding carries them (wit-component's `package-docs`), and a host uses them
  only when the package matches the component's types exactly.

### Discovery and loading

A host can let an agent find and add tools during a session. It does this with
two tools of its own, `find_tools` and `load_tools`, listed beside the others;
the names are reserved. Loading is a host decision under the host's policy.
Nothing in the WIT changes for this path: it is ordinary tool calls, and new
tools appear in the next `tools.list`, so a harness that lets its model load
tools lists them again before its next model call.

A harness can also do the same itself. `tool-discovery` and `tool-loading`
(0.4.4, unstable behind the `agent-spawn` feature) are the harness-facing form
of the same mechanism, with one catalog, one admission and one session
ceiling: a person's approval lasts for the session, a load serves the run in
progress, and the host keeps the model's tool list within a per-model function
budget by setting aside the least recently called tools. `spawn`, in the same
group, starts child agent sessions in a tree under the caller's session,
addressed by session id so a child may run on another host, and bounded so
a child's reach never exceeds its parent's. See
[docs/spawn](docs/spawn/README.md) and its
[rationale](docs/spawn/RATIONALE.md).

## Session states

`session-status` (0.4.3, unstable behind the `session-status` feature) names where a session stands, so every host reports
it the same way: a desktop app's session list, a cluster's session API, and
a component that drives an agent.

| State | When |
| --- | --- |
| `needs-input` | The host holds something for a person: a tool call or outbound request waiting to be allowed, a tool waiting to be added, a change waiting for review. |
| `working` | A run or a compaction is in progress, and nothing is held for a person. |
| `failed` | Nothing is in progress, and the last run ended with an error other than cancellation, or was cut off. |
| `done` | Nothing is in progress, and the last run ended with a reply. |
| `idle` | Nothing is in progress, and no run has ended since the session began or was cleared, or the last run was cancelled. |

The host owns the state and derives it from its own facts: the run it is
driving, the approvals it holds at its gates, and the outcome it received.
An agent can't set it, and a model can't either. `needs-input` comes from the
host's approval state, never from a reply that asks a question: a run that
finishes `input-required` is `done`, and the person's answer starts the next
run.

The set of states will grow. Until a minor release names a new state, a
host reports it as `other` with its name, and a caller treats it as unknown.

A component reads the state with `get` and follows it with `watch`, through
the `status-client` world. Like `session` and `run`, it never names a
session: the host binds the one it reports. It stays unstable until more
than one host adopts it, so its shape can still change, for example to let
`needs-input` say what is held.

## Versioning

Every change to this package lands here first, as a pull request, before
hosts and agents adopt it.

- A breaking change bumps the minor version (0.4.0 to 0.5.0). Adding a case to
  a variant, a field to a record or a function to an interface a guest exports
  (`tools`, `run`) is breaking for compiled consumers.
- An additive change bumps the patch version (0.4.0 to 0.4.1) and carries
  `@since` for the new items. New interfaces, and new functions on interfaces
  the host serves, are additive.
- Published artifacts never change. Interfaces that may still move are behind
  `@unstable` features (`agent-run`, `session-status` and `agent-spawn` today).
- Versions never contain "draft".
