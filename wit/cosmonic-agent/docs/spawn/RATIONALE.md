# Spawn and tool loading: rationale

Why `tool-discovery`, `tool-loading`, `lineage`, `child-sessions` and `spawn` look the way they do,
for people implementing them or reviewing the interfaces.
The WIT comments say what a caller can rely on;
[CONFORMANCE.md](CONFORMANCE.md) says what a host and an implementer must do;
[README.md](README.md) walks through scenarios.
Everything is unstable and cheap to change before `@since`.

## 1. Security model

### 1.1 A child never exceeds its ancestors

A child agent's reach (what it can read and send data to)
never exceeds what its ancestors hold plus what a grantor approved for that child.
This is checked again each time the child starts a run or loads a tool component.
Anything beyond a caller's ceiling waits on a grantor: policy, an auditor, or a person.

### 1.2 Reach comes from the binary

Reach is a component's grants limited to the interfaces it actually imports,
read from the binary before it runs and shown to whoever approves it.

### 1.3 One grantor and enforcer

The implementer is the only grantor and enforcer:
it serves every import, dispatches every tool call,
meters every model call and holds every budget.

### 1.4 What this does not claim

Redaction is not a boundary, since a model can paraphrase anything it read.
A child's final reply is a declassification channel by nature.
Nothing here blocks prompt injection.

## 2. Shape

### 2.1 Interfaces

| Interface | The guest may | Gate |
| --- | --- | --- |
| `tool-component-types` | (types only: grants, tool component sources, states) | `agent-tool-loading` |
| `tool-discovery` | ask which tool components are available | `agent-tool-loading` |
| `tool-loading` | load tool components into itself and set them aside | `agent-tool-loading` |
| `spawn-types` | (types only: child state, runs, budgets) | `agent-spawn` |
| `lineage` | read its own place in the tree and its remaining budget | `agent-spawn` |
| `child-sessions` | see the sessions below it | `agent-spawn` |
| `spawn` | start child sessions and drive them | `agent-spawn` |

### 2.2 One power per interface

Each interface grants one power, so a component's import list says what it may do.
A leaf agent can read its remaining budget (`lineage`) without being able to spawn,
and an agent can be given `spawn` without a separate grant to watch (`child-sessions`),
or a leaf the right to read its budget.

### 2.3 `child-sessions` stays apart from `lineage`

Every spawned child may read its own lineage: where it sits and what budget it has left.
Seeing and watching the sessions below it is a separate power a parent may or may not hand on.
Merging the two would make that an implementer's runtime choice
instead of something visible in the import list.

### 2.4 Gates

Tool loading needs no leases, trees or budgets,
so it has its own types and its own gate, and can stabilize on its own schedule.
`agent-spawn` builds on `agent-tool-loading`, `agent-run` and `session-status`.

### 2.5 No worlds of their own

A world that only bundles imports saves a few lines
but hides which interfaces a component actually imports,
and the import list is the contract.
A component imports the interfaces it needs into its own world;
the README lists the usual imports for each kind of component.

### 2.6 Tool and tool component

A tool is one function the model calls.
A tool component is one component, with its own grants, that offers tools.
Loading, setting aside, approval and reach all apply to a whole tool component,
so the types name it that way.

## 3. Decisions

### 3.1 Waiting on a grantor is a state, not an error

A spawn beyond the caller's ceiling returns the child at once in state `pending`,
holding no instance and no reach.
The grant admits it in place, after re-checking everything it rests on,
and a refusal or an expired request ends it.
A load waits as `awaiting-approval`.

Earlier drafts returned an approval handle inside an error and asked the caller to resend the identical request,
with a one-time ticket consumed by the first resend.
That needed a canonical request digest in a guest-facing comment
and several rules about tickets, resends and coalesced waiters.
As state, "one approval admits one child" is structural,
there is nothing to resend, and `approval` and `decision` are gone.

### 3.2 One approval covers one child, not its descendants

What was approved for a child counts as beyond its ceiling when its own children ask,
so they are refused unless every higher ancestor holds it.
Reading it the other way would let one approval of a child that holds `spawn`
spread to a whole subtree nobody was asked about.

### 3.3 Reach is grants intersected with imports

A component can call only what it imports, and the import list is in the binary.
The implementer refuses a component that imports something its grants do not cover,
drops grants for interfaces it does not import before checking the ceiling or asking anyone,
and shows the import list to the grantor and in discovery offers.
A tool that cannot open a socket is then a property of the binary a person can be shown, not a promise.
This also replaces two special-case refusals with one rule.

### 3.4 State on the child, not a notice queue

A child runs one run at a time, started only by its parent or a person,
so what a parent needs at the start of its next run is the current state of each child's last run,
not a stream of events delivered once.
`session-info.last-run` carries it; `watch-children` covers the in-run case.
The read-and-acknowledge protocol, sequence numbers and gap flag are gone.

### 3.5 Children are named by the caller and acted on through a handle

The name, scoped to the spawning component and session, is the child's durable address and its idempotency key:
spawning a name that exists returns that child (or `name-taken` if the request differs),
and run names do the same for `send`.
A `child-session` resource is a reference for acting on it:
dropping it releases the reference only, `end` consumes it,
and `get-child` finds the child again from any later run or machine.
Records (`session-info`) serve listing, so reads are batched rather than one call per field.

Earlier drafts used host-minted ids with separate idempotency keys and "an id authorizes nothing" rules;
a name the caller chose plus a handle leaves nothing to forge.
An even earlier draft had a handle whose drop ended the child, which cannot outlive one parent run;
this one is a reference, not an owner.

### 3.6 Illegal states are unrepresentable

`agent-source` and `tool-component-source` are variants (a catalog entry or a description),
so no field must be left empty and a catalog entry cannot be edited by the caller.
A catalog entry is a record with a named field, so it says what the string is and can grow.
`output-shape` replaces a schema option plus a `structured-only` flag.
`child-state` replaces three fields with eight combinations.
`child-run` is a record, so its two halves are named.

### 3.7 Each function has its own error

Errors are per function or per interface
(`spawn-error`, `send-error`, `load-error`, `unload-error`, `child-sessions-error`),
each naming only what can be returned.

### 3.8 Listing and watching stay two functions

`list-children` returns a list and `watch-children` a stream of events.
One function would need a variant result, which is clumsier to call.
Both take a `child-query` record instead of a bare flag,
so filters can be added later without breaking either.

### 3.9 Discovery returns a list

`list-offers` returns a short ranked list with a hard cap:
it is what the model chooses from in one step.
A stream would add an async protocol without letting the caller do anything sooner.
If discovery ever covers a large or remote registry,
a paged or streamed variant can be added beside it.

### 3.10 Implementer obligations live in a conformance document

Leases, fencing, the store's consistency, metering, the stopping barrier and the admission snapshot
are things a caller cannot observe directly.
Keeping them in WIT comments made generated bindings hard to read
and asked a test double for a linearizable store.
CONFORMANCE.md holds them with two profiles:
a single-machine implementer meets the lease and fencing rules trivially.
Comment volume fell by about 60 percent.

### 3.11 One session per workload

These imports take no session handle,
so the implementer attributes each call by workload and component,
which is unambiguous only when a workload serves one session for its whole life.
Tool components are components of the workload,
so a shared workload would also share tool components and removals across sessions.
The cost is density: sessions of one agent cannot share warm instances.
The alternative, a session context on every call including `tools.call`,
keeps sharing possible but widens every interface and every tool.
This is the starting point, not the final word.

### 3.12 Hierarchical budgets, not reservations

A child's budget is a ceiling,
and siblings together may exceed their parent's remainder, as cgroup v2 and KeyKOS meters work.
Splitting budgets by reservation would close the sibling-spend channel by construction
but forbid overcommit and need refunds;
coarse ancestor figures are the cheaper fix for now.

### 3.13 `spawn-budget` beside `run-options`

`run-options` has `max-steps` and `max-total-tokens`,
but those limit one run and the run's driver sets them.
`spawn-budget` is the parent's cap over the child's whole life and its subtree, across all its runs.
`run-options` is already released, so it stays as it is;
the implementer keeps each run's limits within the budget.

## 4. Cut and kept

### 4.1 Cut

- Placement hints: now operator policy.
- The trace-parent argument: the implementer knows the calling run's trace.
- The caller-chosen alias prefix: operator configuration.
- `load`'s wait argument: `load` now resolves when the tool component is callable,
  will be next run, or waits on a grantor;
  a caller that wants a shorter wait drops the call and the load goes on.
- Labels: the name identifies a child.
- Numeric defaults in type comments: a table in CONFORMANCE.md.
- The request digest's canonical form: an implementer detail now that nothing is resent.

### 4.2 Kept from earlier rounds

- A child never exceeds any ancestor, rechecked on use and by provenance.
- A load approval lasts for the session, and only a person removes it.
- One session per workload.
- Stopping reaches the subtree, behind a barrier, forced after a grace period.
- Every root has a finite budget, budgets cover the subtree, and remaining budgets are coarse across subtrees.
- A lost machine fails the run instead of hanging, with leases and fencing whose limits are stated plainly.
- A load serves the run in progress or says `next-run`.
- `set-aside` keeps the approval, and the tool list has a budget per model.
- Tool components are dispatched, never linked.
- `structured-only` output is built by the implementer.

## 5. Where this departs from the WIT design guidance

### 5.1 Virtualization

The guidance asks that any import may be served by a peer component sitting in the middle.
Here caller identity is ambient (the calling workload, component and session),
and a guest component placed in front of the implementer would become the caller and its grants the ceiling,
so policy layers are installed by the host rather than composed as guests.
A test double can still export any of these interfaces.
Comments speak of the implementer and the caller, not the host.

### 5.2 Shared types interfaces

They are named `<topic>-types`, as elsewhere in the package,
since one package cannot hold several interfaces named `types`.

### 5.3 `spawn` is a verb

It follows `run` in this package and `wasi:cli/run`.

### 5.4 Catch-alls

The new interfaces use `other(option<string>)` as the guidance asks;
the rest of the package uses `other(string)`, to be aligned at the next breaking version.

### 5.5 Grants by name

Volumes, secrets, credentials and models are named by string rather than labeled imports,
because requests are built at run time;
the import-list rule ties each grant back to the world.

## 6. Not adopted

### 6.1 Detached children

A child's authority rests on its ancestors.

### 6.2 Streaming a child's text to the parent

It would skip the output shape and size checks.

### 6.3 Conversation handoffs

They change reach without the person's approval.

### 6.4 Automatic restarts

A restarted agent repeats its effects.

### 6.5 A session context on every import

See 3.11.

### 6.6 Fencing every effect at the store

A partitioned machine cannot reach the store to check.

### 6.7 Resuming a lost run elsewhere

Effects cannot safely be replayed.

### 6.8 Grant handles a parent narrows and hands down

A parent cannot pass its imports to a child in another workload,
and requests beyond the ceiling must describe reach the caller does not hold.
Worth revisiting when runtimes can narrow a running instance's reach.

## 7. Still open

### 7.1 Records growing after stabilization

Resources grow by methods;
records returned in bulk should stay small and grow by new functions.
A builder resource is the escape hatch for `spawn-request`.

### 7.2 Narrowing a running instance's reach

It needs runtime support.

### 7.3 Goals and swarms

A host-checked "run until done" loop needs a session-level wait and ownership rules.
Peer messaging needs membership as a power a `structured-only` child cannot hold.
A guard against re-sending a child that keeps failing,
and input schemas for catalog agents, are notes for then.
