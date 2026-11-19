# Spawn and tool loading: rationale

Why `tool-discovery`, `tool-loading`, `lineage`, `children` and `spawn` look
the way they do, for people implementing them or reviewing the interfaces.
The WIT comments say what a caller can rely on; [CONFORMANCE.md](CONFORMANCE.md)
says what an implementer must do; [README.md](README.md) walks through
scenarios. Everything is unstable and cheap to change before `@since`.

## Security model

A child agent's reach (what it can read and send data to) never exceeds what
its ancestors hold plus what a grantor approved for that child, and this is
checked again each time the child starts a run or loads a toolset. Reach is
a component's grants limited to the interfaces it actually imports, read from
the binary before it runs and shown to whoever approves it. Anything beyond
a caller's ceiling waits on a grantor: policy, an auditor, or a person. The
implementer is the only grantor and enforcer: it serves every import,
dispatches every tool call, meters every model call and holds every budget.
Redaction is not a boundary, since a model can paraphrase anything it read; a
child's final reply is a declassification channel by nature; nothing here
blocks prompt injection.

## Shape

| Interface | The guest may | Gate |
| --- | --- | --- |
| `toolset-types` | (types only: grants, toolset sources, states) | `agent-tool-loading` |
| `tool-discovery` | ask which toolsets are available | `agent-tool-loading` |
| `tool-loading` | load toolsets into itself and set them aside | `agent-tool-loading` |
| `spawn-types` | (types only: child state, runs, budgets) | `agent-spawn` |
| `lineage` | read its own place in the tree and its remaining budget | `agent-spawn` |
| `children` | see the sessions below it | `agent-spawn` |
| `spawn` | start child sessions and drive them | `agent-spawn` |

Each interface grants one power, so a world's import list says what a
component may do. Tool loading needs no leases, trees or budgets, so it has
its own gate and can stabilize on its own schedule. `agent-spawn` builds on
`agent-tool-loading`, `agent-run` and `session-status`.

## Decisions

**Waiting on a grantor is a state, not an error.** A spawn beyond the
caller's ceiling returns the child at once in state `pending`, holding no
instance and no reach; the grant admits it in place, after re-checking
everything it rests on, and a refusal or an expired request ends it. A load
waits as `awaiting-approval`. Earlier drafts returned an approval
handle inside an error and asked the caller to resend the identical request,
with a one-time ticket consumed by the first resend; that needed a canonical
request digest in a guest-facing comment and several rules about tickets,
resends and coalesced waiters. As state, "one approval admits one child" is
structural, there is nothing to resend, and `approval` and `decision` are
gone. An approval also covers that child only, not its descendants: a
grandchild that asks for what was approved for its parent needs its own
approval. Reading it the other way would let one approval of a child that
holds `spawn` spread to a whole subtree nobody was asked about.

**Interfaces split by grantable power.** Observing a subtree (`children`),
reading one's own budget (`lineage`) and creating and driving children
(`spawn`) are separate imports, so a leaf agent can read its remaining
budget without being able to spawn, and an agent can be given `spawn`
without a separate grant to watch or a leaf the right to read its budget.
Tool loading has its own types and gate.

**Reach is grants intersected with imports.** A component can call only what
it imports, and the import list is in the binary. The implementer refuses a
component that imports something its grants do not cover, drops grants for
interfaces it does not import before checking the ceiling or asking anyone,
and shows the import list to the grantor and in discovery offers. A tool
that cannot open a socket is then a property of the binary a person can be
shown, not a promise. This also replaces two special-case refusals with one
rule.

**State on the child, not a notice queue.** A child runs one run at a time,
started only by its parent or a person, so what a parent needs at the start
of its next run is the current state of each child's last run, not a stream
of events delivered once. `child-info.last-run` carries it; `watch-children`
covers the in-run case. The read-and-acknowledge protocol, sequence numbers
and gap flag are gone.

**Children are named by the caller, and acted on through a handle.** The
name, scoped to the spawning component and session, is the child's durable
address and its idempotency key: spawning a name that exists returns that
child (or `name-taken` if the request differs), and run names do the same
for `send`. A `child` resource is a reference for acting on it; dropping it
releases the reference only, `end` consumes it, and `get-child` finds the
child again from any later run or machine. Records (`child-info`) serve
listing, so reads are batched rather than one call per field. Earlier drafts
used host-minted ids with separate idempotency keys and "an id authorizes
nothing" rules; a name the caller chose plus a handle leaves nothing to
forge. An even earlier draft had a handle whose drop ended the child, which
cannot outlive one parent run; this one is a reference, not an owner.

**Illegal states are unrepresentable, and each function has its own error.**
`agent-source` and `toolset-source` are variants (a catalog name or a
description), so no field must be left empty and a catalog entry cannot be
edited by the caller. `output-shape` replaces a schema option plus a
`structured-only` flag. `child-state` replaces three fields with eight
combinations. Errors are per function (`spawn-error`, `send-error`,
`load-error`, `unload-error`), each naming only what that function can
return.

**Implementer obligations live in a conformance document.** Leases, fencing,
the store's consistency, metering, the stopping barrier and the admission
snapshot are things a caller cannot observe directly. Keeping them in WIT
comments made generated bindings hard to read and asked a test double for a
linearizable store. CONFORMANCE.md holds them with two profiles: a
single-machine implementer meets the lease and fencing rules trivially.
Comment volume fell by about 60 percent.

**Cut.** Placement hints (now operator policy), the trace-parent argument
(the implementer knows the calling run's trace), the caller-chosen alias
prefix (operator configuration), `load`'s wait argument (`load` now resolves
when the toolset is callable, will be next run, or waits on a grantor; a
caller that wants a shorter wait drops the call and the load goes on), labels (the
name identifies a child), numeric defaults in type comments (a table in
CONFORMANCE.md), and the request digest's canonical form (an implementer
detail now that nothing is resent).

**Kept from earlier rounds.** A child never exceeds any ancestor, rechecked
on use and by provenance. A load approval lasts for the session, and only a
person removes it. One session per workload. Stopping reaches the subtree,
behind a barrier, forced after a grace period. Every root has a finite
budget, budgets cover the subtree, and remaining budgets are coarse across
subtrees. A lost machine fails the run instead of hanging, with leases and
fencing whose limits are stated plainly. A load serves the run in progress or
says `next-run`. `set-aside` keeps the approval, and the tool list has a budget per
model. Toolsets are dispatched, never linked. `structured-only` output is
built by the implementer.

**One session per workload.** These imports take no session handle, so the
implementer attributes each call by workload and component, which is
unambiguous only when a workload serves one session for its whole life.
Toolsets are components of the workload, so a shared workload would also
share toolsets and removals across sessions. The cost is density: sessions of
one agent cannot share warm instances. The alternative, a session context on
every call including `tools.call`, keeps sharing possible but widens every
interface and every tool.

**Hierarchical budgets, not reservations.** A child's budget is a ceiling,
and siblings together may exceed their parent's remainder, as cgroup v2 and
KeyKOS meters work. Splitting budgets by reservation would close the
sibling-spend channel by construction but forbid overcommit and need refunds;
coarse ancestor figures are the cheaper fix for now.

## Where this departs from the WIT design guidance

- **Virtualization.** The guidance asks that any import may be served by a
  peer component sitting in the middle. Here caller identity is ambient (the
  calling workload, component and session), and a guest component placed in
  front of the implementer would become the caller and its grants the
  ceiling, so policy layers are installed by the host rather than composed
  as guests. Provider worlds still exist, so a test double can implement each
  interface. Comments speak of the implementer and the caller, not the host.
- **Shared types interfaces** are named `<topic>-types`, as elsewhere in the
  package, since one package cannot hold several interfaces named `types`.
- **`spawn` is a verb.** It follows `run` in this package and `wasi:cli/run`.
- **Catch-alls.** The new interfaces use `other(option<string>)` as the
  guidance asks; the rest of the package uses `other(string)`, to be aligned
  at the next breaking version.
- **Grants by name.** Volumes, secrets, credentials and models are named by
  string rather than labeled imports, because requests are built at run
  time; the import-list rule ties each grant back to the world.

## Not adopted

- **Detached children.** A child's authority rests on its ancestors.
- **Streaming a child's text to the parent.** It would skip the output shape
  and size checks.
- **Conversation handoffs.** They change reach without the person's approval.
- **Automatic restarts.** A restarted agent repeats its effects.
- **A session context on every import.** See one session per workload.
- **Fencing every effect at the store.** A partitioned machine cannot reach
  the store to check.
- **Resuming a lost run elsewhere.** Effects cannot safely be replayed.
- **Grant handles a parent narrows and hands down.** A parent cannot pass its
  imports to a child in another workload, and requests beyond the ceiling
  must describe reach the caller does not hold; worth revisiting when
  runtimes can narrow a running instance's reach.

## Still open

- **Records growing after stabilization.** Resources grow by methods;
  records returned in bulk should stay small and grow by new functions. A
  builder resource is the escape hatch for `spawn-request`.
- **Narrowing a running instance's reach** needs runtime support.
- **Goals and swarms.** A host-checked "run until done" loop needs a
  session-level wait and ownership rules; peer messaging needs membership as
  a power a `structured-only` child cannot hold. A guard against re-sending a
  child that keeps failing and input schemas for catalog agents are notes for
  then.
