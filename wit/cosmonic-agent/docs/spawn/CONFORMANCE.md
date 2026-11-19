# Spawn and tool loading: conformance

What an implementer of `tool-discovery`, `tool-loading`, `lineage`,
`children` and `spawn` must do. The WIT comments say what a caller can
observe and rely on; this page says how an implementer keeps those promises.
[RATIONALE.md](RATIONALE.md) says why. Everything is unstable.

"Must" is a requirement for any implementer that serves real agents. A test
double or a policy layer in front of a conforming implementer may skip the
mechanics but must not contradict what a caller observes.

## Profiles

| Requirement | Single-machine implementer | Implementer that places sessions across machines |
| --- | --- | --- |
| Session store | Local, durable across restarts | Shared and linearizable (compare-and-swap) for leases, generations, names, approvals and limit reservations; a zone that cannot answer consistently answers `unavailable` |
| Leases and fencing | Met trivially: a run and its store share a process; a restart ends every run in progress as `host-lost` | Required as below |
| Re-placement delay | Not needed | Required |
| Brokered calls | Checked locally | A broker away from the run checks the generation with the store and fails closed |
| Placement | One machine | Operator policy (labels, zones); never a caller input |

Everything else applies to both.

## Caller identity and sessions

- **One session per workload.** Serve these imports only to a workload bound
  to one session for its whole life; answer `disabled` otherwise. None of the
  imports takes a session handle, so the implementer attributes each call by
  the caller's workload incarnation (minted at creation, kept across rollouts
  and restarts) and component name, and maps them to the session. On a
  runtime that gives host plugins the caller's workload and per-workload bind
  configuration, put the session id there when creating the workload.
- **Run context.** The implementer that drives a session's runs passes the
  plugin, for each call, the calling run's id, fencing generation, deadline
  and trace context. The plugin uses them for the stopping barrier, fencing,
  budget checks and trace propagation. A root session not driven through
  `run` takes its context from whatever drives it (a trigger or a person's
  request), with its own deadline and no parent generation.
- **Policy layers** sit in front of the implementer, installed by the host,
  and pass the original caller through. A guest component placed between a
  caller and the implementer would become the caller, so there is no
  guest-side policy world (see the virtualization note in the rationale).

## Reach and the ceiling

- **Reach is grants intersected with imports.** Read a component's import
  list from the pinned binary before running anything. Refuse (`invalid`) a
  component that imports a capability its baseline and grants do not cover.
  The baseline is the role's world: `agent` for an agent component (plus
  the client worlds of the powers it was given), and for a toolset only
  function-less type interfaces. A grant for an interface the component does
  not import is dropped before the ceiling check and before asking anyone,
  and leaves the admitted snapshot, so it is never applied. An import the
  implementer cannot classify (an unfamiliar interface, or a bare function
  import) counts as using every kind of grant. A child component that
  imports a power its request does not give it is `invalid`. Show the import
  list to the grantor and in discovery offers.
- **What imports cannot show.** An import list says what a component can
  call directly, not where data flows through those calls (tool results,
  shared volumes) or how an interface is configured. Every agent imports
  `chat`, so its prompts reach its model's provider; for a quarantined
  reader, use a model binding without provider-run tools or network access.
- **Fetching images.** Read imports only from registries the operator
  allows, with size and time caps and a pull rate per caller, and answer
  every fetch failure the same way (`invalid`), so a caller cannot probe the
  implementer's network position.
- **Annotations never feed reach.** Tool annotations such as `read-only` are
  publisher claims; they never feed reach, admission or the ceiling.
- **Credentials.** An implementer without a credential broker treats every
  credential reference as beyond the ceiling.
- **No agent powers by grant.** Refuse an `interface-grant` naming a
  `cosmonic:agent` interface. Powers reach a child only through
  `agent-powers`, and only if the caller holds them.
- **The ceiling** is the caller's own grants (containment) plus its toolset
  entries (exact match): declared toolsets, a child's spawned toolsets, and
  the session ceiling. An entry's reach never counts as the caller's own.
- **Containment.** A volume fits a caller's mount of the same storage
  (names are never compared); a read-only request fits a read-only or
  read-write mount, a read-write request only a read-write one.
  Allowed hosts, after normalization (lowercase, A-labels, one trailing dot
  stripped, default ports dropped): `*` contains everything; a bare `h`
  contains `h` and `scheme://h` at the default port for `http`, `https`,
  `ws`, `wss`; `h:p` contains `h:p` and `scheme://h:p`; `scheme://h[:p]`
  contains only itself; `*.d` contains hosts ending in `.d` at any depth,
  never `d`. A model alias fits one the caller was granted. A grant whose
  reach cannot be compared does not fit.
- **Location-dependent hosts.** Loopback, link-local and private-range
  addresses, single-label names and cluster-local names mean different
  services on different machines. A child whose grants (or toolsets') name
  one runs on its parent's machine, and any child runs only where its
  snapshot resolves identically (the same storage, bindings and model).
- **Within the ceiling,** admit at once only digests in the operator's
  catalog or matching an entry; send any other digest to a grantor.
- **Toolsets are dispatched, never linked.** A toolset's exports are reached
  only through dispatch; its imports are served only by its own grants. Refuse
  a toolset the runtime would link to a sibling (for example a tool provider
  under a served-within-workload rule).

## Approvals

- **Snapshot.** Before asking, resolve the request into an immutable
  snapshot: the storage behind each volume, the binding behind each
  credential, the secret behind each name, the model behind the alias, an
  agent definition's content, each interface grant's resource, every
  executable digest and import list, and every default the request left open.
  Show it to the grantor; never return it to the caller. Admitting re-resolves
  and compares; any difference asks again.
- **Spawn.** A child's toolset beyond the parent's ceiling goes to a grantor
  only if the ceiling of every ancestor above the parent covers it (none, for
  a root parent); otherwise `spawn` answers `denied`. Record that dependency
  for later re-checks. The child exists from the first accepted `spawn`, in
  `pending`, holding no instance and no reach; its first run waits as
  `blocked(admission)`, and that wait never counts as active time. One
  approval admits one child: it is a property of that session, so there is
  nothing to resend or consume. `max-duration` starts at admission.
- **Admission is one atomic step.** Granting a pending child re-checks, in
  one step: a fresh snapshot, current policy, the parent's and every
  ancestor's current ceiling, the dependencies recorded at spawn, limits and
  budgets, and the stopping mark. If any fails, the child is not admitted
  and its state says why. A run deferred while pending is re-checked the
  same way before it starts.
- **Ending a pending request.** A refusal, the operator's approval lifetime
  running out (recommended 24 hours), or `end` while pending ends the child
  (`ended(some(denied))` or `ended(some(withdrawn))`), frees its place,
  keeps its name, withdraws the prompt, and fails its waiting runs with
  `not-admitted`. A pending load the caller sets aside is withdrawn the same
  way. Removal of an entry, or any narrowing of an ancestor, a pending child
  rests on withdraws it too.
- **Load.** The toolset waits as `awaiting-approval`; a later identical
  `load` returns that state. A grant admits it and adds it to the **session
  ceiling**: an entry keyed to the caller, the toolset and its snapshot. A
  later identical load is within the ceiling, re-checked against the snapshot
  and current policy each time.
- **A spawn's approved toolsets** become the child's own entries, bound to
  that child's request (output shape, size limit, budget, powers). They never
  join the parent's ceiling, so the parent cannot load them and another child
  cannot use them. They do not extend to the child's own descendants: when a
  descendant's toolset matches only an entry approved for its parent's
  admission, it counts as beyond the parent's ceiling, so the ancestor gate
  above applies: `denied` unless every higher ancestor covers it, and then a
  grantor is asked as for the descendant's own request. The grantor summary
  for a child given `spawn` says so.
- **Removal.** Only a person removes a session ceiling entry, in the host's
  UI. Removal withdraws what rests on it: pending loads and pending children
  that name or rest on it, a rollout adding it (not published; the load
  reports `denied`), its binding, calls to it in flight, and descendants'
  admissions that rested on it. Withdrawal takes effect at the next dispatch
  as well as at the next run acceptance. The
  session's revocation generation, advanced by each removal, only detects
  races: a step that started before a removal re-checks that its own entry
  still exists. The host UI lists every entry, bound or not.
- **Cooldown and caps.** After a refusal or removal, matching requests (same
  executable digests and resolved reach, whatever the names) answer `denied`
  without a prompt for the cooldown. A root's refusal covers the tree; a
  descendant's covers its subtree. Pending approvals are capped per caller,
  per child subtree, and per tree, with one place kept for the root, and
  prompts are rate-limited per subtree whatever they ask, so varying one
  host cannot flood the person.
- **Who decides.** The grantor is a person, or policy a person wrote. An
  auditor is installed by host policy and is never a session in the
  requesting tree; a spawned agent never holds approval authority.
- **Grantor summaries** use catalog names, image references confirmed
  against the registry, and caller-chosen names as quoted identifiers limited
  to `[a-z0-9-_./:]`. Never caller free text. A summary is shown to the
  requesting caller only; below the caller's direct children, a pending
  state carries the approval id alone.

## Children

- **Names are never reused.** A child's name is scoped to the spawning
  component in its session, and a run's name to its child; both are
  `[a-z0-9-]`, 1 to 64 characters. Match the name before any other check, so
  a retry of a run in progress returns that run. Keep a record of every name
  used, with the request and first run (or message) it was
  accepted with, for the life of the caller's session, so a retry is always
  recognized and a handle can never reach a different child. Outcomes are
  kept separately: at least the 16 most recent per child; `wait` on an older
  run answers `failed(expired)`. Ended children stay listed for a retention
  window (recommended the 100 most recent per tree), and `get-child` finds
  an ended child by name for the session's life; `wait`, `cancel` and `end`
  on it are ok.
- **Joint acceptance.** A child and its first run are accepted in one atomic,
  durable step, including their limit reservations. If either cannot be
  accepted, neither is. A retry compares both the request and the first
  run; any difference is `name-taken`.
- **Powers.** A child that receives `spawn` also gets `children`. Every
  spawned child may import `lineage`.
- **Authority by provenance.** For each of a child's grants and entries,
  record what it rested on: an ancestor's grant or entry, or a grantor's
  approval of this child. At each run acceptance and each child load,
  re-check against that authority as it stands and against current policy. A
  toolset that no longer fits is withdrawn; a child whose own grants no
  longer fit is `stopped(revoked)`. A child's own load beyond its ceiling
  goes to a grantor only if every ancestor's ceiling covers it; otherwise
  `denied`.
- **Lifetime.** A child ends on `end`, its `max-duration`, its parent
  session's end, or when the spawning component's identity ends (workload
  deleted or recreated). Each of these stops the child's subtree as Stopping
  describes.
- **Outputs.** Validate every outcome against its output shape and
  `max-bytes`, whatever the shape; `output-rejected` never quotes the
  rejected output. With `structured-only`, return only the validated
  structured value and build `finish`, `model` and counts from the
  implementer's own records; the schema is not checked for free strings.
  Copy stored attachments across the boundary and re-issue them in the
  receiving session; they count toward `max-bytes`. Enforce `max-runs` and
  `max-steps` by the implementer's own count, never the child's.
- **Nothing a child chose reaches the grandparent.** For sessions below the
  caller's direct children, return no name, no `status.run`, and a pending
  state with the approval id only.
- **Size caps.** Cap instructions, schemas and messages (operator settings),
  answering `invalid`.
- **Handles.** Keep one backing object per caller and name, recording the
  owning caller identity; every method checks it, so a handle passed to
  another component addresses nothing. `end` consumes the handle even when it
  fails. If it fails before marking the subtree, nothing changed. Once the
  mark is set, ending is durable: the implementer completes it whatever
  happens to the call, and a repeated `end` (through `get-child`) is
  idempotent and resolves when it is done.

## Runs, leases and fencing

For implementers that place sessions across machines.

- **Every run holds a lease** with a fencing generation: root runs, runs a
  parent starts, and runs a person starts by talking to a child. Each counts
  against budgets and limits and is stopped by its ancestors' `cancel`.
- **The store decides expiry** (recommended 60 seconds after the last
  renewal). The generation closes when the run ends or the lease expires;
  the store then rejects any write, outcome or dispatch it mediates under
  that generation. Whichever machine next reads the child reports the run
  `host-lost`; no particular machine has to be alive.
- **Self-fencing.** The running machine stops the run's instances (agent and
  toolsets) and cancels calls in flight once its monotonic clock shows the
  store's timeout minus a margin (recommended 15 seconds) since it *sent* its
  last renewal that succeeded. A late reply does not revive a fenced run.
- **Brokered calls** (model calls, credential-brokered egress, tool dispatch,
  writes through a host-managed store) carry the generation. A broker on the
  run's machine admits them while that machine's fence has not fired; a
  broker elsewhere checks the store and fails closed.
- **Re-placement delay.** After expiry, the child's next run is accepted but
  starts only after a delay counted from expiry (at least the self-fence
  margin, recommended 30 seconds), shown as `blocked(placement-delay)`. The
  waiting run holds its concurrency place and counts toward `max-duration`.
- **Journal completion.** After a run that did not finish, once its
  generation is closed and before a new lease opens, append the closing
  entries the run could not: `agent.op-result.v1` with outcome `unresolved`
  for each tool op with no result, then `agent.run-result.v1` with outcome
  `cancelled` or `failed` and why. Nothing is replayed.
- **What fencing does not stop.** Effects already under way when the fence
  fires, and a machine whose clock misbehaves (a paused virtual machine), can
  overlap a new run, within the child's own reach. Direct egress through a
  component's own grants cannot be fenced.

## Stopping

- **Barrier.** `cancel`, `end` and a person's stop first mark the whole
  subtree as cancelling. The required outcome is that no run acceptance in
  the subtree succeeds after the mark until the stop completes: `spawn` and
  `send` from inside (`stopping`, `stopped`), `send` from outside (`busy`),
  and runs a person or trigger would start. A store with per-key
  compare-and-swap meets it with one per-tree record that every acceptance
  already updates for its reservation.
- **Force.** Cancel cooperatively; after the grace period (recommended 15
  seconds) stop each remaining run's instances as for self-fencing, stop
  metering, and refuse that run's lease renewals, telling the renewing
  machine to stop. A machine the request cannot reach is stopped by its
  lease.
- **Completion.** `cancel` and `end` resolve when every run in the subtree
  has ended or lost its lease: within the grace period plus the lease
  timeout. Effects already under way are not undone.

## Budgets and limits

- **Every root is finite.** A token budget over the whole tree (the root's
  own calls included) and an active-time budget (time during which any run
  in the tree is in progress). An operator may raise them, never remove them.
- **Subtree charging.** A session's spend counts against it and every
  ancestor. When a budget is spent, runs below it finish `max-total-tokens`
  or `max-duration`, and the sessions below it are `stopped(exhausted)` until
  an operator or person raises it. A child's own `max-duration` is different:
  it is measured from admission (never while `pending`), and passing it ends
  the child.
- **Metering.** The broker serving model calls reserves the estimated prompt
  plus the call's maximum output before each call, refuses when any budget on
  the path cannot cover it, and settles real usage afterwards. A broker may
  instead draw down an allowance it holds locally, as long as the sum of
  allowances never exceeds a budget. A provider
  that reports no usage is charged the estimate. Spend is recorded outside
  the run's generation, so a fenced run's spend still counts.
- **Remaining budgets** are exact for the caller's own limits and coarse for
  ancestors' (rounded down to a power of two tokens or a whole minute,
  refreshed at most once a minute); a root's ancestor figures repeat its own,
  coarse. `over-limit` reports the configured limit, never a live count.
- **`max-spawns`** limits spawning only. Its default is the operator's,
  computed once when the request is first checked and fixed in the snapshot.
- **Reservations.** Every spawn and run acceptance reserves its place
  atomically.

### Defaults

| Setting | Default |
| --- | --- |
| Depth | 3 |
| Runs in progress per tree | 20 (fail fast, never queue) |
| Live children per tree | 50 |
| Children spawned per tree | 500 |
| Toolset components bound per agent workload | 16 |
| Approval lifetime (pending, undecided) | 24 hours |
| `max-spawns` when unset | a quarter of what is left of the tree's total |
| Pending approvals | 3 per caller, 3 per child subtree, 5 per tree |
| Refusal cooldown | 1 hour |
| Offered functions | 32 hosted frontier models, 20 unknown, 10 local models up to about 14B parameters |
| Rollout preparation | 2 minutes, then abandoned |
| Residency before set-aside | 3 runs |
| Lease timeout, self-fence margin, re-placement delay | 60 s, 15 s, 30 s |
| Cancel grace period | 15 s |
| `watch-children` streams | 4 per caller |

## Tool loading

- **Same run where possible.** Tools are dispatched, so a new toolset can be
  callable while the caller's instance keeps running: roll the workload out
  to a revision that adds the component, route calls to it there, and retire
  the old revision when the run ends. An implementer that cannot do this
  reports `next-run`. Before publishing, check the toolset's entry or
  admission still stands. `load` resolves at `offered`, `next-run`,
  `awaiting-approval` or an error, so a caller never polls.
- **Rollout deadline.** Abandon a rollout not prepared within the deadline:
  the toolset becomes `failed`, frees its place, and a waiting `load` answers
  `unavailable`. A revision prepared for the next run is kept until that run
  starts on it.
- **Function budget.** From the model binding (`max-offered-functions`),
  which the manifest may only lower; the smallest among the caller's model
  aliases. Set aside the least recently called toolset the caller loaded
  (never called first, then oldest), never a pinned one or one loaded or
  called within the residency window. Unbind only at the component cap, only
  set-aside toolsets with no call in flight.
- **Aliases** are fixed for the session and never reused for another
  toolset's tool. A call to a set-aside toolset's alias answers
  `unknown-tool`. Record which digest each alias denoted in the tool list
  sent with each model call, and refuse a call whose alias now denotes
  another.
- **Results** are bounded as `tool-types.tool-response` describes.

## Resources served across stores

The only resource here is `spawn.child`, and it carries nothing but the
child's name and owner: all state is in the session store. On a runtime
where a resource served by a component host plugin is proxied into the
caller's store, a caller that crashes holding a handle leaves the plugin's
side registered until the plugin restarts. wash-runtime documents this in
`crates/wash-runtime/src/engine/store/resource_bridge.rs`, module
documentation, section "Known limitations", item "Caller crash leaks reals"
(wasmCloud/wasmCloud main). An implementer should therefore keep one small
plugin-side object per caller and name, and reclaim those whose caller's
workload has stopped. A native host plugin, whose resources live in the
caller's own store, has no such leak. Durability never depends on a handle:
`get-child` reaches the same child by name from any run or machine.

## Audit and traces

- Record every admission (catalog, entry or grantor), refusal, removal,
  withdrawal, set-aside, unbind, cancel, end and lease expiry, each naming
  what it rested on.
- Start each child run in the calling run's trace (W3C `traceparent` only,
  never `tracestate` or baggage): as a child span while the calling run is in
  progress, a linked span after. Use the session id as the conversation id.

## Conformance cases

Each case names the property it checks.

1. **One approval, one child.** Spawn a beyond-ceiling child: it is
   `pending`, with no instance. Spawn the same name and request again: the
   same child, still one approval. Grant it: `live`. Spawn another name with
   the same request: a new approval. Deny one: `ended(denied)`, its waiting
   first run fails `not-admitted`, its place is freed, and a matching request
   in the cooldown answers `denied`.
2. **The parent and grandchildren gain nothing.** After approving a child's
   toolset that the root lacks, the parent's `load` of it waits on a
   grantor, a second child using it waits too, and the child's own child
   using it is `denied`.
3. **Admission re-checks.** While a child is pending, narrow the parent's
   grants, remove an entry it rests on, change policy, or start a stop: a
   later grant does not admit it, and its waiting run does not start.
4. **Session ceiling.** Approve a load. Set it aside and load again:
   `offered` with no prompt. Have the person remove it: the next load waits on
   a grantor. Remove it during its rollout: the rollout is not published.
   Rebind its volume or publish a new digest: it waits again.
5. **Reach from imports.** A toolset whose component imports no HTTP or
   socket interface but is granted allowed hosts: the hosts are dropped before
   the ceiling check. A component importing a capability nothing grants:
   `invalid`. An interface grant naming `cosmonic:agent/spawn`: `invalid`.
6. **Never beyond an ancestor.** A child whose agent grants exceed the
   parent's: `denied`. Narrow the parent's grants: the child's next `send`
   answers `stopped`, and its state is `stopped(revoked)`. Remove a parent's
   entry a child's toolset rested on: the child loses it.
7. **Names and retries.** Send run `r1`, drop the reply, send `r1` again with
   the same message: one run. With another message: `name-taken`. Drop the
   handle, call `get-child` from a fresh instance: the same child, `wait` on
   `r1` returns its outcome.
8. **State, not notices.** Start runs in three children and end the caller's
   run. The next run's `list-children` shows each child's `last-run` with its
   state.
9. **Subtree stop.** Cancel a child whose agent ignores cancellation: after
   the grace period its instances stop, metering stops, renewals are refused,
   and `cancel` resolves. During the stop, a `send` into the subtree answers
   `busy` and a spawn from inside answers `stopping`.
10. **Lost machine.** Cut off a child's machine mid-run: within the lease
    timeout its `last-run` is `failed(host-lost)`; a later write from that
    machine is rejected; the next run shows `blocked(placement-delay)`; the
    journal ends the interrupted run with `unresolved` op results.
11. **Budgets.** Spend in a grandchild counts against its parent and root.
    Spend a root's tokens: runs below finish `max-total-tokens` and children
    are `stopped(exhausted)`. A sibling's `get-lineage` shows only coarse
    ancestor figures.
12. **Structured only.** A child that writes text in its reply, finish
    reason and model name: the caller sees only the structured value and the
    implementer's own fields.
13. **Sessions apart.** The same component in two sessions on a host that
    shares workloads: `disabled`. Two sibling components: neither sees the
    other's children.
14. **Same-run loading.** `load` returns `offered` and the tool is callable in
    the same run, or returns `next-run` and the tools are in the next run's
    `tools.list`. A hung rollout answers `unavailable` after the deadline and
    the toolset shows `failed`.
15. **Handles.** End a child through its handle: the handle is consumed and
    the child ends. Crash a caller holding handles: the children are
    unaffected and reachable by name. Pass a handle to another component: it
    addresses nothing.
16. **No child-chosen text upward.** A child with `spawn` names its own runs
    and children with arbitrary text: the grandparent's `list-children(true)`
    shows no names, no `status.run`, and pending states with ids only.
17. **Disabled powers.** On a host that shares workloads between sessions,
    every function answers `disabled`.
