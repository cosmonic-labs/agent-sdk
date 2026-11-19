# Spawn and tool loading: rationale

This page explains the choices behind `spawn`, `tool-discovery` and
`tool-loading` for people implementing a host or reviewing the interfaces. The
rules themselves are normative in the WIT comments, starting with
`spawn-types.wit`; [README.md](README.md) walks through scenarios. Everything
here is unstable and cheap to change before the items are marked `@since`.

## Security model

A child agent's reach (what its grants let it read and send data to) never
exceeds that of any ancestor. Its own grants must fit inside its parent's own
grants, and the model it uses must be one the parent was granted. Anything
more travels as a toolset: a separate component whose reach is recorded as an
exact entry and never lent to another component. A request beyond the caller's
ceiling (its own grants plus the toolsets it holds) answers `needs-approval`.
An approval is an explicit declassification (a decision that data may cross a
boundary it otherwise could not) by a person or host policy, bound to a digest
of the request and to a snapshot of what its names resolved to: the storage
behind each volume, each credential and secret, the model, every executable
digest. A renamed or rebound request is a new request and asks again. The host
is the only grantor and the only enforcer: it serves every import, dispatches
every tool call, meters every model call, holds every budget, and re-checks a
child's admission each time the child starts a run or loads a toolset.
Redaction is not a boundary, since a model can paraphrase anything it read; a
child's final reply is a declassification channel by nature; and nothing here
blocks prompt injection.

## Decisions

**Children are sessions, addressed by id.** A child has its own workload,
conversation and state, and the parent names it by session id through the
host's session store rather than holding an in-process handle. A child can
then run on another host and outlive the parent's current run, and dropping a
stream or future only stops listening; `cancel` and `end` are the explicit
ways to stop it.

**A parent acts on its direct children and sees its subtree.** Authority
never skips a level, so a parent cannot drive a grandchild, but it can list
the whole tree with each session's state. Children are bound to the component
that spawned them, so a sibling component cannot see or drive them, and they
end when that component's identity ends.

**A child never exceeds any ancestor, rechecked on use.** Checking only at
spawn would let the invariant decay as the tree changes. The host records
what each of a child's grants and toolsets rests on and re-checks the whole
admission at every run acceptance and load: a toolset that no longer fits is
withdrawn, and a child whose own grants no longer fit is `revoked`.

**One spawn approval admits one child.** The approval is a one-time ticket
consumed by the request's idempotency key, and the toolsets it approves become
that child's own entries, tied to the output schema, size limit and budget the
person was shown. The parent cannot load them and a later, less constrained
child cannot reuse them.

**A load approval lasts for the session.** Approving a toolset load adds it
to the session's ceiling, so reloading the identical toolset (after it was set
aside, unbound or rolled back) does not ask again. The entry admits only its
exact snapshot and is re-checked against current policy on each use. Only a
person removes it, and removal withdraws everything resting on it, including
calls in flight and descendants' admissions.

**One session per workload.** These imports take no session handle, so the
host attributes each call by workload and component, which is unambiguous
only when a workload serves one session for its whole life; a host that
shares workloads gets `disabled`. Toolsets are components of the workload, so
a shared workload would also share toolsets, rollouts and removals across
sessions. The cost is density: sessions of the same agent cannot share warm
instances, though idle instances are cheap and idle reclaim recovers much of
the rest.

**Stopping reaches the subtree.** `cancel`, `end` and a person's stop mark the
whole subtree as cancelling first, so nothing in it can start new work during
the stop. Cancellation is cooperative, then forced after a grace period, and
resolves only when every run below has ended, as
[OTP supervisors](https://www.erlang.org/doc/system/sup_princ.html) shut
children down. A run that ends any other way leaves its children's runs going
so a later run can collect them.

**Every root has a finite budget, and budgets cover the subtree.** Tokens
spent anywhere below a session count against it and every ancestor, as
[cgroup v2](https://docs.kernel.org/admin-guide/cgroup-v2.html) charges a
child group to its parents. The root's token and active-time budgets can be
raised but never removed. The broker serving model calls reserves an estimate
before each call, so overshoot is bounded. A session sees its own remainder
exactly but its ancestors' only coarsely, since exact figures would leak
siblings' spend.

**A lost host fails the run instead of hanging.** Every run holds a lease with
a fencing generation (a counter the store advances so stale writers are
rejected). The store expires the lease on its own clock and rejects anything
it mediates from the old generation, so `wait` reports `host-lost` without the
lost host's help. The running host stops the child's instances on its own
monotonic clock shortly before the store would expire it, and brokered calls
carry the generation. A new run waits out a re-placement delay before it
starts.

**What fencing cannot stop.** Effects already under way when the fence fires
(a request sent, a write issued) and a host whose clock misbehaves (a paused
virtual machine) can overlap a new run, limited to the child's own reach.
Direct egress through a component's own grants cannot be fenced. See
[How to do distributed locking](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html)
for why only the resource can reject a stale holder.

**A load serves the run in progress, or says it cannot.** Tools are
dispatched by the host, not linked to the agent, so a new toolset can become
callable while the agent's instance keeps running; the old revision retires
when the run ends. A host that cannot drain yet may make the toolset callable
from the next run instead, and `load-result.next-run` says so, so the harness
ends its run rather than polling. A pending approval returns at once and never
holds the run.

**`unload` sets aside, and the tool list has a budget per model.** Setting a
toolset aside removes its definitions from the model's prompt but keeps its
component and approval, so bringing it back is instant. Tool selection gets
worse as the list grows and small local models need a much shorter one, so
the host keeps offered functions within a budget from the model binding and
sets aside the least recently called toolset when a load needs room.

**Toolsets are dispatched, never linked, and hold no agent powers.** A
sibling linked straight to a tool would bypass dispatch, revocation and audit,
and a toolset importing `spawn` or `tool-loading` would gain powers its agent
was never given. Both are refused.

**Retries are safe.** `spawn` requires an idempotency key and `send` accepts
one, so a caller whose reply was lost can resend without starting a second
run. A known key whose outcome has aged out answers `expired`, never a new run.

**`structured-only` output is built by the host.** When set, the parent
receives only the validated structured value; every other outcome field comes
from the host's own records, so a child cannot hide text in a finish reason or
model name.

**Limits are always set.** Defaults per tree, all adjustable by the operator:

| Limit | Default |
| --- | --- |
| Depth | 3 |
| Runs at once | 20 |
| Live child sessions | 50 |
| Children spawned in total | 500 |
| Pending approvals | 3 per caller, 3 per child subtree, 5 per tree |
| Offered functions | 32 hosted frontier models, 20 unknown, 10 small local models |
| Load wait | 30 seconds; rollout preparation 2 minutes |
| Lease timeout | 60 seconds; self-fence margin 15 seconds; cancel grace 15 seconds |

After a denial, the same request (matched by digest and resolved reach, not
by name) is refused without a prompt for a cooldown, so a looping agent
cannot flood the person.

## Not adopted

- **Detached children.** A child's authority rests on its ancestors, so a
  child that outlives them would hold reach nothing vouches for.
- **Streaming a child's text to the parent.** Streamed text skips the output
  schema and size checks; the parent gets contentless progress and the final
  outcome.
- **Conversation handoffs.** Passing the person's conversation to another
  agent changes reach without the person's approval.
- **Automatic restarts.** A restarted agent repeats its effects and spends
  again; restarts belong to a future goal loop with its own budget.
- **Confined spawn** (results only to a sink the parent cannot read). The use
  cases are served by ordinary spawn, brokered credentials or a narrow tool.
- **A session context on every import.** It would keep workload sharing
  possible but needs a handle threaded through every call, including
  `tools.call`, and toolsets would still be shared.
- **Fencing every effect at the store.** A partitioned host cannot reach the
  store to check, so it would fail every call.
- **Resuming a lost run elsewhere.** A run's effects cannot safely be
  replayed; a later `send` starts a new run.
- **A tool-change stream.** The harness calls `tools.list` again after a load.

## Still open

- **Records growing after stabilization.** Records the guest builds and the
  host returns cannot gain fields after `@since`; whether they become
  resources, gain accessors or carry an extension field is undecided.
- **Narrowing a running instance's reach.** Taking an allowed host away from a
  running instance, and narrowing a child's egress as it reads untrusted
  content, need runtime support that does not exist yet. Until then such
  changes apply at the next run or dispatch.
- **Goals and swarms.** A host-checked "run until done" loop and peer
  messaging between agents would build on spawn; their shape, and a guard
  against re-sending a child that keeps failing, are not decided.
