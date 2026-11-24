# Spawn and tool loading: rationale

Why `tool-discovery`, `tool-loading`, `session-tree` and `spawn` look the way they do,
for people implementing them or reviewing the interfaces.
The WIT comments say what a caller can rely on;
[CONFORMANCE.md](CONFORMANCE.md) says what a host and an implementer must do;
[README.md](README.md) explains the model and walks through common tasks.
Everything is unstable and cheap to change before `@since`.

Each decision below is stated in its first sentence, followed by why.

## 1. Security model

### 1.1 A child never exceeds its ancestors

A child agent's reach (what it can read and send data to)
never exceeds what its ancestors hold plus what a grantor approved for that child.
This is checked again each time the child starts a run or loads a tool component,
by provenance: each of the child's grants and tool components records what it rested on
(an ancestor's grant or entry, or a grantor's approval of this child),
and that authority is checked as it stands now.
Anything beyond a caller's ceiling (what it may add without asking)
waits on a grantor: policy, an auditor, or a person.

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
| `session-tree` | read the session tree it belongs to: where it sits, its remaining budget, and the sessions below it | `agent-spawn` |
| `spawn` | start child sessions and drive them | `agent-spawn` |

### 2.2 One power per interface

Each interface grants one power, so a component's import list says what it may do.
A leaf agent can read its place and remaining budget (`session-tree`) without being able to spawn,
and only a component that imports `spawn` can start or drive a session.

### 2.3 Reading the session tree is one power, acting on it is another

`session-tree` holds every read of the session tree,
in both directions: the caller's own place and remaining budget, and the sessions below it.
`spawn` is the only power that acts on the tree.
This merges what would otherwise be two read interfaces, one looking up and one looking down,
because the line that matters for what a component can do is reading versus acting.

- **Reading changes nothing.** Every record it returns is the implementer's own,
  and below the caller's direct children it carries nothing a child chose (CONFORMANCE 7.15).
- **Splitting reads protects nothing.** A caller sees only the sessions its own component spawned,
  so a child without `spawn` sees an empty list,
  and a separate interface for looking down would only lengthen the import list.
- **It is a familiar shape.** An OS process tree works the same way:
  a process reads its own parent and lists its children,
  and starting or signalling a process is a separate call.

### 2.4 Tool loading has its own gate

Tool loading has its own types and its own feature gate, and `agent-spawn` builds on it.
Tool loading needs no leases, trees or budgets,
so it can stabilize on its own schedule.
`agent-spawn` also builds on `agent-run` and `session-status`, whose types it uses.

### 2.5 No worlds of their own

These interfaces come with no worlds; a component imports the ones it needs into its own world.
A world that only bundles imports saves a few lines
but hides which interfaces a component actually imports,
and the import list is the contract.
The README lists the usual imports for each kind of component.

### 2.6 Tool and tool component

The types name the tool component, not the tool, as the unit that is loaded and approved.
A tool is one function the model calls;
a tool component is one component, with its own grants, that offers tools.
Loading, setting aside, approval and reach all apply to a whole tool component.

## 3. Decisions

### 3.1 Waiting on a grantor is a state, not an error

A spawn beyond the caller's ceiling returns the child at once in state `pending`,
holding no instance and no reach, and a load waits as `awaiting-approval`.
The grant admits the child in place, after re-checking everything it rests on,
and a refusal or an expired request ends it.
As a state, "one approval admits one child" is structural:
the caller has nothing to resend,
and no ticket, request digest or resend rule needs to appear in a guest-facing comment.

### 3.2 One approval covers one child, not its descendants

What was approved for a child counts as beyond its ceiling when its own children ask,
so they are refused unless every higher ancestor holds it.
Reading it the other way would let one approval of a child that holds `spawn`
spread to a whole subtree nobody was asked about.

### 3.3 Reach is grants intersected with imports

The implementer refuses a component that imports something its grants do not cover,
drops grants for interfaces it does not import before checking the ceiling or asking anyone,
and shows the import list to the grantor and in discovery offers.
A component can call only what it imports, and the import list is in the binary,
so a tool that cannot open a socket is a property of the binary a person can be shown, not a promise.
One rule also covers cases that would otherwise each need a special refusal.

### 3.4 State on the child, not a notice queue

What a parent needs about a child is the current state of the last run it started there,
carried in `session-info.last-run`; `watch-children` covers the in-run case.
A child runs one run at a time, started only by its parent or a person,
so at the start of its next run a parent needs where each child stands,
not a stream of events delivered once.
State also needs no read-and-acknowledge protocol, sequence numbers or gap flag.

### 3.5 Children are named by the caller and acted on through a handle

The name, scoped to the spawning component and session, is the child's durable address and its idempotency key.
Spawning a name that exists returns that child (or `name-taken` if the request differs),
and run names do the same for `send`.
A `child-session` resource is a reference for acting on it:
dropping it releases the reference only, `end` consumes it,
and `get-child` finds the child again from any later run or machine.
Records (`session-info`) serve listing, so reads are batched rather than one call per field.

A name the caller chose plus a handle leaves nothing to forge,
where host-minted ids would need separate idempotency keys and rules that an id authorizes nothing.
A handle is a reference, not an owner,
because a handle whose drop ended the child could not outlive one parent run.

### 3.6 Illegal states are unrepresentable

The types are variants and named records wherever a flag or an empty field would allow nonsense.
`agent-source` and `tool-component-source` are variants (a catalog entry or a description),
so no field must be left empty and a catalog entry cannot be edited by the caller.
A catalog entry is a record with a named field, so it says what the string is and can grow.
`output-shape` is one variant rather than a schema option plus a `structured-only` flag.
`child-state` is one variant rather than three fields with eight combinations.
`child-run` is a record, so its two halves are named.

### 3.7 Each function has its own error

Errors are per function or per interface
(`spawn-error`, `send-error`, `load-error`, `unload-error`, `session-tree-error`),
so each names only what can be returned.

### 3.8 Listing and watching stay two functions

`list-children` returns a list and `watch-children` a stream of events.
One function would need a variant result, which is clumsier to call.
Both take a `child-query` record instead of a bare flag,
so filters can be added later without breaking either.

### 3.9 Discovery streams its offers

`list-offers` returns a stream of offers, best match first,
and a future that reports an error,
the shape of `read-directory` in WASI 0.3's `wasi:filesystem`.
The caller reads as many offers as it wants and drops the stream to stop.
The cap on what a model sees lives in the host's `find_tools` adapter (CONFORMANCE 2.8).

- **Callers differ.** A model chooses from one answer in one step, so it needs a cap.
  Agent code that filters or ranks offers itself should not be cut off at a number chosen for a model.
- **One shape for both.** The adapter reads one offer past its cap to tell the model that more matched;
  code reads on. Neither needs a page token or a `truncated` flag.
- **A large or remote catalog can be produced lazily.**
  The implementer computes only what the caller reads.
- **Errors keep their place.** An error before any offer and an error midway both arrive on the future,
  so a short stream is never mistaken for a complete one.

### 3.10 Implementer obligations live in a conformance document

Leases, fencing, the store's consistency, metering, the stopping barrier and the admission snapshot
are in CONFORMANCE.md, not in WIT comments.
A caller cannot observe them directly.
In WIT comments they would make generated bindings hard to read
and ask a test double for a linearizable store.
CONFORMANCE.md holds them with two profiles,
so a single-machine implementer meets the lease and fencing rules trivially.

### 3.11 One session per workload

A workload that is offered these imports serves exactly one session for its whole life.
These imports take no session handle,
so the implementer attributes each call by workload and component,
which is unambiguous only under this rule.
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
coarse ancestor figures (3.15) are the cheaper fix.

### 3.13 `spawn-budget` beside `run-options`

`spawn-budget` is the parent's cap over the child's whole life and its subtree, across all its runs.
`run-options` has `max-steps` and `max-total-tokens`,
but those limit one run and the run's driver sets them.
`run-options` is released, so it stays as it is;
the implementer keeps each run's limits within the budget.

### 3.14 A load approval lasts for the session

A person's approval of a tool component holds until the person removes it.
Setting a tool component aside to make room in the function budget
should not cost the person another prompt when it comes back,
and only a person, in the host's UI, should be able to remove it.

### 3.15 Every root is finite, and remaining budgets are coarse across subtrees

Every root session has a token and an active-time budget, budgets cover a session and its whole subtree,
and ancestor remainders are reported coarsely.
A tree that can spawn would otherwise have no bound on what it spends.
Exact ancestor figures would let one subtree read how much its siblings spend.

### 3.16 Stopping reaches the subtree

`cancel` and `end` stop every run below the child,
behind a barrier that refuses new runs, and by force after a grace period.
A child's children act on authority that rests on it, so they stop with it.
The barrier keeps a new run from starting while the stop is under way,
and force bounds a child that ignores cancellation.

### 3.17 A lost machine fails the run instead of hanging

A run whose machine is lost ends `failed(host-lost)`,
and leases and fencing keep the old machine from writing after that.
A parent can then decide what to do, rather than wait on a run that will never finish.
The limits of fencing (effects already under way, a misbehaving clock) are stated plainly in CONFORMANCE 8.7.

### 3.18 A load serves the run in progress, or says `next-run`

An implementer that can add tools to a running session does so,
and one that cannot answers `next-run`.
An agent that finds a tool halfway through a task usually needs it in that task,
and an honest `next-run` lets its harness end the run instead of waiting for tools that will not come.

### 3.19 Set-aside keeps the approval, and the function budget is per model

`set-aside` removes a tool component's tools from the model's list but keeps its component and approval,
and the budget on offered functions comes from the model binding.
Models choose less well among too many functions, and how many is too many depends on the model,
so the budget belongs to the binding and making room must be cheap to undo.

### 3.20 Tool components are dispatched, never linked

The caller reaches a tool component only through the implementer's dispatch.
Linking would join the tool component's reach to the agent's,
while dispatch keeps each component's grants its own
and lets the implementer see, meter and withdraw every call.

### 3.21 The implementer builds `structured-only` output

With `structured-only`, the parent receives the validated structured value
and fields (`finish`, `model`, counts) the implementer fills from its own records.
Nothing the child chose to write, other than the value its schema allows,
then reaches the parent.

## 4. Where this departs from the WIT design guidance

### 4.1 Virtualization

Policy layers are installed by the host rather than composed as guests.
The guidance asks that any import may be served by a peer component sitting in the middle,
but here caller identity is ambient (the calling workload, component and session),
and a guest component placed in front of the implementer would become the caller and its grants the ceiling.
A test double can still export any of these interfaces.
Comments speak of the implementer and the caller, not the host.

### 4.2 Shared types interfaces

They are named `<topic>-types`, as elsewhere in the package,
because one package cannot hold several interfaces named `types`.

### 4.3 `spawn` is a verb

It follows `run` in this package and `wasi:cli/run`.

### 4.4 Catch-alls

These interfaces use `other(option<string>)` as the guidance asks,
while the rest of the package uses `other(string)`;
the two are to be aligned at the next breaking version.

### 4.5 Grants by name

Volumes, secrets, credentials and models are named by string rather than by labeled imports,
because requests are built at run time;
the import-list rule ties each grant back to the world.

## 5. Considered and not adopted

### 5.1 Detached children

A child's authority rests on its ancestors, so it cannot outlive them.

### 5.2 Streaming a child's text to the parent

It would skip the output shape and size checks.

### 5.3 Conversation handoffs

They change reach without the person's approval.

### 5.4 Automatic restarts

A restarted agent repeats its effects.

### 5.5 A session context on every import

See 3.11.

### 5.6 Fencing every effect at the store

A partitioned machine cannot reach the store to check.

### 5.7 Resuming a lost run elsewhere

Effects cannot safely be replayed.

### 5.8 Grant handles a parent narrows and hands down

A parent cannot pass its imports to a child in another workload,
and requests beyond the ceiling must describe reach the caller does not hold.
Worth revisiting when runtimes can narrow a running instance's reach.

### 5.9 An approval returned in an error, with a resend

The caller would resend the identical request with a one-time ticket.
That needs a canonical request digest in a guest-facing comment
and rules about tickets, resends and coalesced waiters; a state needs none (3.1).

### 5.10 Placement hints from the caller

Placement is operator policy (labels, zones), never a caller input.

### 5.11 A trace-parent argument

The implementer already knows the calling run's trace.

### 5.12 A caller-chosen alias prefix

The prefix on model-facing tool aliases is operator configuration.

### 5.13 A wait argument on `load`

`load` resolves when the tool component is callable, will be on the next run, or waits on a grantor.
A caller that wants a shorter wait drops the call, and the load goes on.

### 5.14 Labels on children

The name already identifies a child.

### 5.15 Numeric defaults in type comments

Defaults are an implementer's and operator's choice; CONFORMANCE.md lists recommended ones in a table.

### 5.16 A canonical form for the request digest in the contract

Nothing is resent, so comparing requests is an implementer detail.

## 6. Still open

### 6.1 Records growing after stabilization

Resources grow by methods;
records returned in bulk should stay small and grow by new functions.
A builder resource is the escape hatch for `spawn-request`.

### 6.2 Narrowing a running instance's reach

It needs runtime support.

### 6.3 Goals and swarms

A host-checked "run until done" loop needs a session-level wait and ownership rules.
Peer messaging needs membership as a power a `structured-only` child cannot hold.
A guard against re-sending a child that keeps failing,
and input schemas for catalog agents, are notes for then.
