# Spawn, tool discovery and tool loading: design notes

**Status: unstable, behind the `agent-spawn` feature.** The interfaces are
`spawn-types`, `spawn`, `tool-discovery` and `tool-loading` in
`cosmonic:agent` 0.4.4, with the worlds listed in [README.md](README.md).
They moved here from cosmonic-labs/awesome-cosmonic#76, where they were
drafted as a separate `cosmonic:agent@0.5.0` directory outside the build.
These notes record the rules, the decisions behind them, and what
implementing them needs. Each decision says what was chosen, why, and what
else was considered.

Two powers, kept apart:

- **Tool loading** (`tool-discovery`, `tool-loading`) adds toolset components
  to the caller's *own* workload and session.
- **Session spawning** (`spawn`) starts *child agent sessions*: other agents,
  each with its own workload, conversation and state, in a tree under the
  caller's session.

They share `toolset-spec`, `admission-error`, the `approval` resource and
the ceiling rules in `spawn-types`.

## What changed from awesome-cosmonic#76

- **Into the real package, at 0.4.4, unstable** (D1).
- **Approvals feed a session ceiling.** A person's approval of a toolset adds
  it to the session's ceiling; loads of the identical toolset are then loads
  within the ceiling. Removal by the person is a revocation barrier (D2).
- **`unload` sets aside** (D3). **Offered, ready and loading states, and a
  function budget** (D4).
- **A load serves the run in progress** (D5, D6).
- **Spawn is hierarchical and location-transparent.** The `agent` resource
  is gone. A child is a session with its own id and a recorded parent;
  the caller sends it input, waits for its result, cancels and ends it by
  session id through the host's session store, and lists its subtree with
  each session's state. A child may run on another host; a lost host fails
  the run with a reason (D7 to D12).
- **Children may receive discovery and loading**, bounded by every
  ancestor's ceiling, with toolset reach kept as exact entries (D8, D9).
- **Smaller shape changes:** `approval` is a resource (D17), an agent can be
  a catalog definition (D13), `placement` (D11),
  `output-spec.structured-only` (D16), `toolset-spec.functions` (D14),
  `discovery-result.truncated` (D15), `progress.cancelling`, the package's
  error style (D19), and the session id in caller identity and the digest
  (D20).
- **Third review round** (an independent Claude review and a fresh Codex
  round): spawn approvals are for one child and add nothing to the parent's
  ceiling, children are re-checked as their ancestors change, one session
  per workload, honest fencing, subtree cancellation and budgets, and the
  fixes listed under "Review" (D2, D5, D7, D8, D10 to D13, D20, D26 to D29).
- **Dropped from #76:** the separate package version and its `wkg.toml`
  override, the `harness` world (now `spawning-agent`), store-scoped child
  handles, and the history of review rounds, which stays in #76.

## Information-flow rules

The WIT states these on `spawn-types`; here is why.

- **Reach** is what a request's grants can read and send data to.
- **The ceiling has two parts that never mix.** A caller's *own grants*
  (its component's grants) admit any new grant they contain, by what it
  reaches: two workloads can call different storage `workspace`, so a
  volume fits only if the same storage backs one of the caller's mounts.
  Its *toolset entries* (declared toolsets, a child's spawned toolsets, the
  session ceiling) admit only an exact match. A toolset's reach is never
  treated as the caller's own, so approving one toolset never lends its
  reach to another component.
- **Beyond the ceiling is `needs-approval`.** An approval is an explicit
  declassification (deciding that data may cross a boundary it otherwise
  could not), bound to a digest over the caller and the request and to the
  admission snapshot of what the request's names resolved to.
- **An approval binds resolved reach, not names.** The snapshot holds the
  storage behind each volume, the credential binding behind each reference,
  the secret behind each name, the model behind the alias, an agent
  definition's content, each interface grant's resource and classification,
  every executable digest, and every default the request left open. The
  person-facing UI shows it; the caller's summary never carries it.
- **No after a no.** After a denial the same request is refused without a
  prompt for a cooldown, and each caller has a capped number of pending
  approvals, so a looping agent cannot flood the person.
- **Downward flow.** Messages and instructions a caller sends a child can
  reach the child's egress. An approval for egress beyond the ceiling says
  so.
- **Plainly:** redaction is not a security boundary, because a model can
  paraphrase anything it read. A child's final reply is a declassification
  channel by nature. Everything else in an outcome is the implementer's own
  (D16). Nothing here blocks prompt injection.
- **Caller identity** is the workload incarnation, the component name and
  the session id, with one session per workload (D20).

## Decisions

Each decision gives its basis: **Eric** where it follows a decision Eric
recorded (with the date), **Eric, Bailey and Victor** for the 2026-11-19
direction on subsessions, **Bailey** where it follows hers, and **decided
here** where this PR chose it after the research cited. Changing any of them
later is cheap while the interfaces are unstable.

### D1. Version and gating: 0.4.4, one feature, stacked on session-status

*Basis: decided here.* The interfaces are in `wit/cosmonic-agent` at 0.4.4,
the next patch after agent-sdk#3 (session-status, 0.4.3, unstable). Adding
interfaces is additive, so a patch release is right under the package's own
versioning rules; nothing an existing agent or tool was built against
changes. This branch is stacked on #3 because `spawn` reports a child's
state with `session-status.status`, and so the two versions cannot collide.

Every item carries `@unstable(feature = agent-spawn)`, the mechanism
`agent-run` and `session-status` already use. Because the interfaces use
`run-types` and `session-status`, a consumer enables `agent-spawn`,
`agent-run` and `session-status` together. The breaking 0.5.0 that
agent-sdk#4 calls for carries these interfaces unchanged.

*Alternatives.* A separate unpublished directory at 0.5.0, as #76 had: two
versions of one package and a `wkg.toml` override to build at all. One gate
per power: all three share `spawn-types` and an item carries one gate, so it
would need a fourth gate that every consumer enables anyway.

### D2. Load approvals feed a session-only ceiling, and removal withdraws what rests on it

*Basis: Eric, 2026-11-18 (session tool set design, Decisions; Cosmonic
Desktop implements the same rule as its session approval envelope). The
ticket and revocation details are decided here, after three review rounds.*

A person's approval of a toolset **load** adds it to the **session
ceiling**: an entry keyed to the caller, the `toolset-spec` digest and the
admission snapshot. A later load of the identical toolset, after the host set
it aside or unbound it, after a rollback, or after a failed rollout was put
back, is a load within the ceiling and needs no prompt.

- **Loads have no separate ticket.** A granted load approval adds the entry
  at once, and any identical load is admitted through it. The "already held"
  rule keeps concurrent identical loads to one rollout.
- **Spawn approvals add nothing to the ceiling** (D8): they admit one child
  and anchor that child's reach only.
- **Re-checked at each use,** in the step that admits it: the request is
  resolved again and compared with the entry's snapshot, and current policy
  is checked again. On Control, admission policy is cluster policy, so a
  session ceiling never becomes a way around a policy change made
  mid-session.
- **Never widens.** An entry admits its exact snapshot only.
- **Only the person removes, and removal withdraws what rests on it.**
  Removal drops the entry, and the implementer withdraws everything resting
  on it: a pending approval for it resolves `expired`, an unconsumed spawn
  ticket naming it is void, a rollout adding it is not published (that load
  answers `denied`), it is unbound, a call to it in flight is cancelled, and
  a descendant's admission that rested on it is withdrawn. Admissions resting
  on other entries are untouched. The session's revocation generation, a
  counter every removal advances, is only a race detector: a step that
  started before a removal re-checks that its own entry still stands, and
  proceeds if it does.
- **Removal starts the cooldown** for that toolset in the session, so the
  model cannot prompt for it again at once.
- **Visible and audited.** The entry lives in the session's state, moves
  with the session and ends with it; the host's UI lists every entry, bound
  or not, so the person can remove any; every admission through it names the
  approval it rests on.

*Why this fits single-use approvals.* Single use protected against N
coalesced requests riding one decision, reflex approval of a repeat, a name
rebound after review, and standing authority hidden in a reusable token. The
first three still hold (spawn tickets per idempotency key, repeat marking,
snapshot comparison). The fourth is answered as #76 already allowed:
standing permission belongs in policy a person wrote. An approval that adds
to a session ceiling is exactly that, a person writing "this session may
hold this toolset at this digest and reach", visible in the prompt ("Your
answer lasts for this session") and removable only by them.

*Alternatives.* Loads after `unload` as new requests that always ask
(defeats setting tools aside); treating a host-initiated unbind as "not an
unload" (bends what an admission means); a ceiling per agent across sessions
(an approval should not outlive the conversation it was given in); a
single-use ticket for loads too (conflicts with the entry); a session-wide
generation that withdraws every earlier admission (over-revokes unrelated
toolsets; review finding 11).

### D3. `unload` sets aside; only a person removes

*Basis: Eric, 2026-11-18.* `unload(name)` moves a toolset the caller loaded
to `ready`: its definitions leave the model's tool list, its component and
its approval stay, and no rollout runs. Loading it again returns it at once.
Nothing in these interfaces removes a toolset from a session: that is the
person's Remove. Pinned and declared toolsets cannot be set aside by the
caller (`denied`). A toolset still `loading` cannot be set aside
(`unavailable`): retry once it is `offered`, so an outstanding `load` always
completes as `offered` or `loading`.

*Alternatives.* `unload` as a rollout that unbinds (the #76 shape): a
rollout each way and a prompt on return, for no gain in safety. A
model-facing unload tool: not in this version; models rarely clean up
unprompted, and the host's budget does it for them.

### D4. Offered, ready and loading states, and a per-model function budget

*Basis: Eric, 2026-11-18 (session tool set design and its five decisions).*

A toolset in a session is **approved** (in the session ceiling), **bound**
(its component is in the workload) and **offered** (its definitions are in
the model's prompt), with one limit each. `toolset-state` reports what a
caller can act on: `loading` (admitted, rollout not finished), `offered`,
and `ready` (bound and approved, not offered). `loaded()` returns a
`working-set` with every toolset bound or loading (declared ones too, so a
harness can count the budget), `offered-functions` and
`max-offered-functions`. A toolset unbound at the component cap leaves the
list and stays in the session ceiling; a late rollout failure drops a
`loading` toolset from the list and an identical load retries.

- **The budget** comes from the model binding (`max-offered-functions`,
  defaulting from the provider family and capped by the provider's hard
  limit, 128 on OpenAI Chat Completions); the workload manifest may only
  lower it. The host's own discovery tools count. Recommended defaults: 32
  for hosted frontier models, 20 for an unknown model, 10 for local models up
  to about 14B parameters.
- **Which model.** The smallest budget among the model aliases the caller
  is granted, for the whole session. *Decided here, after the Codex review:*
  Desktop uses the model the session last called, but a harness that
  switches models could then exceed the new model's budget without another
  load. Most agents are granted one alias, where the two rules agree.
- **Set-aside on load.** When a load would go over the budget, the
  implementer sets aside the least recently called toolset the caller loaded
  (never called before called, then the oldest load), never a pinned one and
  never one loaded or called within the residency window (recommended three
  runs). The result lists what it set aside. Nothing is set aside on a timer.
- **Unbinding** happens only at the component cap (`loaded-toolsets`,
  recommended 16), only to set-aside unpinned toolsets, inside a load's own
  rollout, and never with a call in flight. An idle bound tool costs little
  (compiled code is shared by digest, and a tool with a nonzero idle reclaim
  window holds no instance), so the cap is a prompt and audit limit more than
  a memory one.
- **`over-limit(offered-functions)`** when nothing can make room.

*Why a function count.* Selection accuracy falls as the list grows
(Anthropic: selection "degrades once you exceed 30 to 50 available tools";
OpenAI: "aim for fewer than 20 functions available at the start of a
turn"), small local models need a much smaller list, and providers have hard
caps. A count tracks that better than a cap on components.

*Alternatives.* A hard cap on components (blind to the model and to component
size); a token budget for definitions (tokenizers differ by about two times;
log it, enforce later if count proves too coarse); provider tool search with
deferred loading (provider-specific, nothing for local models; adapters may
still use it for approved tools only).

### D5. A load serves the run in progress

*Basis: goal from Eric, 2026-11-19; mechanism decided here.*

Cosmonic Desktop today rolls the session's workload out when a tool is
loaded, and the new revision serves the agent's next run, which Desktop
starts at once so the turn reads as continuous. The goal is that a load
serves the run that asked for it.

**How.** Tools are linked to nothing: the agent calls them through its
`tools` import, which the host serves, and the host dispatches each call to
the toolset's component by identity. So the agent's instance need not change
for a new toolset to be callable.

1. `load` admits the request (D2, the catalog rule in D18, or
   `needs-approval`).
2. If the toolset is not bound, the implementer rolls the workload out to a
   revision that adds its component. Every new component must start and pass
   its checks; otherwise `load` answers `unavailable`. A component that
   started may have run its own start-up code, which is not rolled back.
3. The run in progress is not moved. The caller's instance keeps running it
   in the old revision, and the session's tool dispatcher routes calls to the
   new toolset in the new revision. The old revision retires once the run
   ends (a drain), and the next run starts on the new revision. The rollout
   publishes only if the toolset's own entry or admission still stands (D2).
4. The toolset becomes `offered` and `load` resolves.

**The deadline.** `load(spec, wait)` resolves when the toolset is callable,
or when `wait` runs out. `wait` is capped by the implementer (recommended 30
seconds) and by what the caller's run has left of its `max-duration`. When
it runs out, `load` returns the toolset as `loading` and the rollout
continues.

**Pending approval never holds the run.** Beyond the ceiling, `load`
answers `needs-approval` at once. The harness finishes the run
(`input-required`) and lets the person's answer start the next one, or keeps
working and loads again once `wait-for-decision` resolves `granted`. A held
approval shows as `needs-input` in the session's state.

**Next-run fallback.** Same-run loading needs the old revision to keep
serving the run while the new one serves calls, and a dispatcher that routes
across both. Desktop today serves the next run, and Control's tools plugin
keys its state by runtime workload and has no revisions yet. So a host that
cannot yet drain may make the toolset callable from the caller's next run:
that is conformant, and `load-result.next-run` says so, so a harness ends its
run instead of polling. Same-run stays the goal and the preferred behaviour.

**Rollout deadline.** Preparing a rollout (pulling, starting and checking
its components) has a deadline (the operator's, recommended two minutes). A
rollout not prepared by then is abandoned: the toolset leaves `loaded()`,
frees its name and place, a joined load answers `unavailable`, and an
identical load starts again. A revision prepared and staged for the next run
is kept until that run starts on it or the toolset is removed, so a pause
before the next run never discards it. `loading` always ends.

*Alternatives.* Keep "next run" semantics with an immediate next run
(Desktop today): the agent loses its in-memory plan and the model sees a
seam. Replace the agent's instance mid-run and resume from the journal: the
`run` contract only promises memory does not survive *between* runs. Run
each loaded toolset as its own workload until the next rollout: splits a
toolset's identity across two homes. Block in `load` until the person
decides: holds the turn indefinitely.

### D6. How a harness learns new tool definitions mid-run

*Basis: decided here.* No new event or stream. Once a toolset is `offered`
the caller's next `tools.list` includes its tools; once `ready`, it leaves
them out. A harness that loads tools, or lets its model call `load_tools`,
calls `tools.list` again before its next model call. The host serves `list`
from its own table without running guest code.

The prompt cache is the host's concern. Changing the tool list at the front
of the prompt invalidates a provider's cache, and on models with preserved
reasoning it can invalidate earlier reasoning too. Anthropic's
mid-conversation tool changes (a `role: "system"` message with
`tool_addition` and `tool_removal` blocks) keep the cached prefix and earlier
reasoning valid; Cosmonic Desktop's Anthropic adapter already sends a
session's tool changes that way. On OpenAI, tools appended at the end keep
the cache up to that point. Aliases never change within a session, which
keeps definitions byte-stable.

*Alternatives.* A `tools-changed` stream: one more thing to wire up, and it
would miss loads made through the model-facing `load_tools`. Returning the
new definitions from `load`: the harness would merge lists itself and could
drift from what the host dispatches.

### D7. Child sessions are first class and hierarchical

*Basis: Eric, Bailey and Victor, 2026-11-19 (spawn was not hierarchical;
make child sessions first class); the shape is decided here.*

A child is an agent session: its own workload, conversation, grants and
state, with its parent's session id recorded by the host. The `spawn`
interface works in session ids:

| Operation | What it does |
| --- | --- |
| `spawn(request, input, key)` | Admits and creates a child session; with `input`, starts its first run. `key` is the caller's idempotency key: a resend with a key that already admitted a child returns that child. |
| `send(session, message)` | Re-checks the child's admission, then starts a run in a direct child. Returns a host-minted run id, a progress stream and an outcome future. |
| `wait(session, run)` | The outcome of that exact run, from any store and host. The host keeps at least the 16 most recent outcomes of runs the caller started in a child, until the child ends. |
| `cancel(session)` | Cancels the child's run in progress and every run in progress below it. |
| `end(session)` | Ends the child session and every session below it. |
| `children(descendants)` | Direct children, or the whole subtree in parent-first order, each with parent, depth, labels (direct children only), `session-status` state, failure reason, location and whether it ended. |
| `get-lineage()` | The caller's own session id, parent and depth. |

- **Acting is on direct children only; seeing is on the subtree.** A parent
  sends, waits, cancels and ends its own children, and lists its
  descendants' state to draw its tree. The host records with each child the
  identity of the component that spawned it and acts only for that
  component, so a sibling component neither sees nor drives another
  component's children. When that identity ceases to exist (the workload is
  deleted or recreated), its children end, so none is orphaned. A session id
  authorizes nothing on its own.
- **Only the caller's own text comes back.** `children` returns labels only
  for direct children; a grandchild's labels were written by a child and
  stay in the host's view.
- **Runs are named,** so a parent that reconnects after a person talked to
  the child still collects its own run's outcome.
- **States reuse `session-status`** (needs input, working, failed, done,
  idle), derived by the host. The reason is `child-failure` (host lost, start
  failed, budget exhausted, revoked).
- **Hosts show the tree** from their session store. A child's approvals
  reach the root session's person, labelled with the child's place.
- **Stopping reaches the subtree, behind a barrier.** `cancel` cancels the
  child's run and every run in progress below it; `end` ends the subtree; a
  person's stop in the host's UI cancels the subtree's runs. Each first marks
  the subtree as cancelling in the store, atomically, and every `spawn` or
  `send` from a run in that subtree (one already in flight included) is
  refused at acceptance, so nothing started during the stop survives it. A run that ends any other way
  (done, failed, host lost) leaves the runs it started in its children
  going: they are durable work a later run can collect, bounded by the tree
  limits and the budgets, which now cover the subtree (D10).
- **Dropping a stream or future only stops listening.** A child may run on
  another host and outlive the caller's store, and a caller that gets a
  fresh store per run must not cancel its children by finishing its own run.
  This differs from the `run` export, where dropping the stream cancels; that
  rule suits a driver that owns one in-process run.

*Alternatives.* The #76 `agent` resource, store-scoped and ending on drop: a
child could not outlive one parent run and was assumed local. Detach and
attach on that resource: a dropped stream still cancelled an active run on
store teardown. Letting a parent act on grandchildren: authority would skip a
level. Cascading every run end to descendants: kills the across-runs pattern
`wait` exists for. Opt-in cascade on stop: a person's Stop must stop the
tree without the agent's cooperation (review finding 8).

### D8. A child's reach rests on its ancestors, and is re-checked as they change

*Basis: Eric, 2026-11-18 (a child spawned with reach beyond its parent's
approval envelope never happens; the subagents milestone: a subagent's
grants are no wider than its parent's, enforced by the host); mechanism
decided here, revised after the third review (findings 1 to 3).*

- **Agent grants fit the parent's own grants,** and the model alias is one
  the parent was granted; otherwise `denied`. Reach the parent lacks goes in a
  toolset ("components follow reach").
- **Toolsets.** A child's toolset that fits the parent's ceiling is
  admitted, resting on the parent's grant or entry it matched. One beyond it
  goes to a grantor only if it fits every ancestor above the parent (none,
  for a root parent); otherwise `denied`.
- **A spawn approval is for one child.** It is a one-time ticket consumed by
  the first resend's idempotency key; coalesced waiters with other keys get a
  new approval, marked as a repeat. It adds nothing to the parent's session
  ceiling. The approved toolsets become the child's own entries, bound to the
  spawn's digest, so they carry exactly the output (schema, `max-bytes`,
  `structured-only`), budget and powers the person was shown. The parent
  cannot load them, and a later child with a wider output cannot use them.
  In the person's terms, the parent's envelope holds "this one child, as
  shown", never the toolset on its own.
- **Ticket consumption re-checks everything** in one step: expiry, a fresh
  snapshot, current policy, every ancestor's current ceiling, and that no
  removal since the grant touched a toolset it names. A removal voids
  unconsumed tickets that name the removed toolset.
- **Provenance and re-checks.** The implementer records, for each of a
  child's grants and entries, the authority it rested on: an ancestor's
  grant or entry, or the person's approval of this exact child. An entry
  resting on that approval is re-checked against the approval (its snapshot,
  and that the toolset was not removed) and the ancestors above the parent,
  never against the parent's ceiling, which never held it (fourth-round
  finding 1). At
  each run acceptance (`send`, `spawn` with input) and each child load, it
  re-checks the child's whole admission against every ancestor's current
  ceiling and policy. A toolset that no longer fits is withdrawn; a child
  whose own grants no longer fit is `revoked` and takes no more runs. A
  person's Remove withdraws dependents at once, by provenance. A change to an
  ancestor's own grants (an operator edit, a policy change) takes effect at
  the next acceptance or dispatch, never mid-call: the runtime cannot narrow
  a running instance's allowed hosts (see "Still not decided").
- **Toolset entries stay toolsets.** A child's toolsets are entries in its
  ceiling, never its own grants, so a child can hand one down exactly but
  never lend its reach to another component.
- **A child's loads follow the same rule:** beyond its ceiling but within
  every ancestor's goes to a grantor, labelled with the child's place;
  beyond any ancestor's is `denied`.

*Alternatives.* The #76 shape, where a person could approve a child beyond
the parent with no record on the parent's side. This PR's second revision,
where a spawn approval added the toolset to the parent's session ceiling:
review finding 1 showed the parent could then load it, and a later child
could use it with no schema and a large `max-bytes`, undoing a quarantine
the person approved. Admission-time checks only: the invariant would hold at
spawn and decay as the tree changed (finding 2).

### D9. Children may receive discovery and loading

*Basis: decided here.* `agent-powers` has `tool-discovery` and
`tool-loading` beside `spawn`. A child receives a power only when its request
asks for it and the parent holds it too. #76 withheld both because loading
had no bound; D8 gives it one. A parent still cannot load a toolset into a
child: it spawns a new child, so a child's tool set never changes under it by
someone else's hand.

### D10. Tree limits, budgets and prompts are bounded by default

*Basis: decided here; answers #76's (y), revised after review finding 9.*

- **Limits always set,** operator-adjustable: `depth` 3, `concurrent-agents`
  20 (runs from acceptance until ended, cancelling included), and
  `live-sessions` 50 (child sessions not yet ended) and `total-agents` 500
  (children spawned in all, which also bounds the idempotency keys a tree
  keeps) per tree. Every spawn and every run
  acceptance reserves its place atomically across the tree; `send` answers
  `start-error.over-limit` when it cannot. Depth and concurrency follow the
  Claude Agent SDK's defaults (`CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH` 3,
  `CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS` 20).
- **Budgets cover the subtree.** Tokens a child's descendants spend count
  against the child's `max-total-tokens` and every ancestor's; a run stops
  (`max-total-tokens`) when any of them is spent. Budgets are still chosen by
  the caller and not carved from the parent's, as in every SDK surveyed, but
  a parent's budget now bounds everything it causes.
- **Prompts are bounded per tree.** The denial cooldown matches requests by
  executable digests and resolved reach anywhere in the tree, not by caller
  or name, so a fresh child, a renamed toolset or a one-byte change to
  `max-bytes` cannot ask again at once. Pending approvals are capped per
  caller (three) and per tree (five).
- **Kept state is bounded:** ended children stay listed up to a retention
  (recommended the 100 most recent per tree), and at least the 16 most
  recent outcomes per child are kept.

### D11. Remote children: location-transparent, with a placement hint

*Basis: Eric, Bailey and Victor, 2026-11-19 (spawning subsessions for remote
sessions is the biggest gap); the shape is decided here.*

- **Nothing assumes the child is local.** Every operation names the child by
  session id and works through the host's session store, so a child may run
  on another host, host group or zone, and move between hosts between runs.
  Control keeps sessions in a store it reconciles across zones; Desktop may
  run everything locally behind the same interface.
- **Placement is a hint** (`any`, `same-host`, a named `host-group`, a named
  `zone`; `other` is reserved and refused as input). It is not in the
  approval digest because it never changes reach: a child runs only where its
  admission snapshot resolves identically. Allowed hosts are names, and some
  names mean different services in different places, so a child whose
  grants (or toolsets') name a loopback, link-local or private-range
  address, a single-label name or a cluster-local name is placed on the
  parent's host whatever the hint (review finding 10). The audit records
  where each child was placed; `child.location` shows it.
- **Streams are proxied.** `send` returns the same progress stream and
  outcome future whether the child is local or remote.
- **Attachments are re-issued** (D27).

*Alternatives.* A remote handle type separate from a local one: doubles the
surface. A mandatory placement: fails spawns for reasons the agent cannot
see; hard residency belongs in policy. Putting placement in the digest:
the prompt would change with a hint the host may ignore.

### D12. A lost host fails the run, never hangs; what fencing does and does not stop

*Basis: Eric, Bailey and Victor, 2026-11-19 (define what happens if a remote
child's host goes away); mechanism decided here, revised after review
finding 5.*

- **The store decides expiry.** A run holds an exclusive lease with a
  fencing generation. The store (or any host serving a read from it) decides
  expiry against its timeout (recommended 60 seconds after the last
  renewal), and expiry closes the generation at once: the store rejects any
  later write, outcome or dispatch it mediates for that run, so `wait` cannot
  change its answer after waiters were told `host-lost`. No particular host,
  not even the parent's, has to be alive for the run to be reported failed.
- **The running host fences itself** on its monotonic clock: once the time
  since it sent its last successful renewal passes the store's timeout minus
  a margin (recommended 15 seconds), it stops the child's instances (agent
  and toolsets), so nothing new can start through any import, model calls,
  tool calls and direct network, volume and interface use alike, and
  cancels what is in flight. A partitioned host cannot consult the store, so
  this is the only fence on its side.
- **A linearizable store.** Leases, generations, spawn tickets and limit
  reservations need compare-and-swap. A store or zone that cannot answer
  consistently (a partition) answers `unavailable`.
- **What fencing does not stop, plainly.** Effects already under way when
  the fence fires (a request already sent, a write already issued) and a
  host whose clock misbehaves (a paused virtual machine that resumes after
  the store expired its lease). Those may overlap a new run. They are
  limited to the child's own reach, never more.
- **The session survives.** A later `send` starts a new run, placed again,
  under a new generation.

*Alternatives.* Fencing every effectful capability on the store (review
finding 5, Codex's form): a partitioned host cannot reach the store to
check, so it would fail closed on every call; self-fencing plus store-side
expiry is what can actually be built. Resuming the lost run elsewhere: a
run's effects cannot safely be replayed. Waiting indefinitely: the silent
hang the direction rules out.

### D13. A child's agent may be a catalog definition, admitted by the same rules

*Basis: decided here; tightened after review finding 7.* `agent-source` is a
pinned `component` with the request's own model, grants and toolsets, or the
name of an agent `definition` in the operator's catalog. A definition is
expanded into its component, alias, grants and toolsets and admitted by
exactly the same rules as a component request: its grants must fit the
caller's own, its toolsets fit or ask, and its alias must be one the caller
was granted. A catalog entry is a convenience, not a privilege, so no agent
can start a more privileged catalog agent and read its reply. A child may
delegate only the alias it was given. The snapshot resolves the name to the
definition's content, so a changed definition is a new request. This answers
#76's (l): a preset is a catalog definition.

### D14. `toolset-spec.functions` trims a toolset

*Basis: decided here.* An optional list of the functions to offer; none
offers all. It is part of the digest. Large components spend the function
budget on functions a job never uses; Cosmonic Control's tools plugin has
the same idea as its `tool-functions` key.

### D15. Discovery says when it truncated

*Basis: decided here.* `list` returns a `discovery-result` with `truncated`.
A capped list without a signal could not be fixed later without breaking
callers.

### D16. `output-spec.structured-only`, enforced by construction

*Basis: decided here; answers #76's (n), in part; tightened after the Codex
review.* True returns the validated `structured` value and nothing else the
child wrote: the implementer empties `reply` and builds every other outcome
field from its own records (`finish` from its budget and metering, `model`
from the chat calls it served, counts from its metering), so a child cannot
hide text in `finish.other` or `model`. It requires a schema. The host does
not check that the schema is closed. Opaque references a parent can pass on
but not read (CaMeL, FIDES) are not added: they need host-side value storage
and provenance that nothing else here has.

### D17. `approval` is a resource, exported by providers

*Basis: decided here; answers #76's (w); provider worlds fixed after the
Codex review.* A record holding a `future` could never gain a field; a
resource with `get-` accessors and `wait-for-decision` can gain methods. The
provider worlds `spawn-provider` and `tool-loading-provider` export
`spawn-types`, because a provider creates the approvals it returns; a world
that imported it could not construct one. Both export it, so in practice
one implementer serves both powers; two separate implementers would give a
guest two `approval` types under one name.

### D18. Within the ceiling, only catalogued code loads without asking

*Basis: decided here; answers #76's (x).* A toolset loaded with the agent's
own hosts turns a latent grant into egress a model chooses. Within the
ceiling, the implementer admits at once only digests in the operator's
catalog or matching a toolset entry; any other digest goes to a grantor.

### D19. The package's error style

*Basis: decided here; answers #76's (j) and (aa).* `denied(string)` and
`other(string)`, as in `tool-types`, `run-types` and `session-status`. One
package, one style.

### D20. One session per workload, and the session in caller identity

*Basis: decided here, after review finding 4.*

These imports are served only to a workload the host bound to exactly one
session for its whole life; a host that binds several sessions to one
workload answers `disabled`. Caller identity is the workload incarnation,
the component name and that session id, and the digest preimage includes
the session.

*Why per-session workloads rather than a session context on the imports.*
Toolsets are components of the workload: a workload shared by sessions
would share their toolsets, instances, rollouts and removals, so a session
context on `spawn` and `tool-loading` alone could not keep sessions apart,
and `tools.call` would need the context too. Both hosts can implement the
rule today. Desktop already gives each agent session its own workload.
Control's tools plugin identifies callers by workload and component id, and
wash-runtime 2.12 gives host plugins the caller's workload id and its
per-workload bind configuration, so the host can put the session id in the
bind configuration when it creates the session's workload and read it on
every call. A session context would need a host-minted handle threaded
through every import, a change to `tools` that guests export, and it would
still leave toolsets shared.

### D21. Discovery shows what could be granted; powers start off

*Basis: #76 defaults 3 and 6, kept.* `list` returns offers within the
ceiling and offers beyond it that policy could approve, each with its fit,
and hides what policy would never grant. Each power is off until the
operator enables it; a power that is off answers the typed `disabled` and is
audited, and the workload still starts.

### D22. The WIT interfaces and the host's discovery tools are one mechanism

*Basis: decided here.* The package reserves `find_tools` and `load_tools`
for a host's model-facing discovery tools. `tool-discovery` and
`tool-loading` are the harness-facing form of the same thing: one catalog,
one admission, one session ceiling, one budget.

### D23. Annotations never feed admission

*Basis: Eric, 2026-11-18, and agent-sdk#4.* Reach comes from grants, never
from a tool's `read-only` or other annotations. How hosts check `read-only`
before passing it through is agent-sdk#4's change to `tool-types`.

### D24. Toolsets live in the agent's workload

*Basis: Bailey, 2026-10-16 (tools are components with the same boundary as
any other); answers #76's (a).* A toolset joins the agent's own workload as a
component, and loading is a rollout attached to the same session.

### D25. Where it is implemented

*Basis: Eric, 2026-11-18.* The interfaces live here; the implementation is
one shared host plugin, `plugin-agent-spawn`, in Cosmonic Control, which
Cosmonic Desktop uses the way it uses Control's typed tool crate.

### D26. Toolsets are dispatched, never linked, and never hold agent powers

*Basis: decided here, after review finding 6.*

- An `interface-grant` may not name a `cosmonic:agent` interface
  (`invalid-request`). Otherwise a toolset could import `spawn` or
  `tool-loading` and become a caller with powers the child was spawned
  without.
- A toolset's exports are reachable only through the host's dispatch, never
  linked to the agent or a sibling, and its imports are satisfied only by its
  own grants. A sibling linked straight to a typed tool's interface would
  bypass dispatch, the revocation check and the audit, and two separately
  approved toolsets could pass data around the model.
- A tool provider (a toolset exporting `cosmonic:agent/tools`) is linked
  straight to the agent's `tools` import by a runtime's served-within-workload
  rule, as Control's runtime does today. A host whose runtime would link a
  toolset's exports refuses that toolset with `invalid-request` until the
  runtime can keep it host-dispatched. Typed tools are unaffected.

### D27. Attachments are re-issued across the parent and child boundary

*Basis: decided here, after review finding 15.* A stored attachment id is
sealed to its session, so one passed in a `send` message or returned in an
outcome would be unusable on the other side. The host copies each stored
attachment the sender can read into the receiving session and re-issues it
there. Outcome attachments count toward `output.max-bytes`; with
`structured-only` an outcome carries none.

### D28. Fit rules

*Basis: decided here, after review finding 14.* Two implementers must agree
on what "fits":

- **Volumes.** A grant fits a caller's mount of the same storage, at the same
  path within that storage or below it. A read-only grant fits a read-only or
  read-write mount; a read-write grant fits only a read-write mount.
- **Allowed hosts,** after normalization:

  | Caller's entry | Contains |
  | --- | --- |
  | `*` | everything (never granted by default) |
  | `h` (bare host) | `h`, and `scheme://h` on that scheme's default port, for `http`, `https`, `ws`, `wss` |
  | `h:p` | `h:p`, and `scheme://h:p` |
  | `scheme://h` or `scheme://h:p` | exactly that scheme, host and port |
  | `*.d` | any host ending in `.d` (any depth), in the same forms as a bare host; never `d` itself |

- **Model aliases** fit an alias the caller was granted.

### D29. What the person is shown is the implementer's, not the caller's

*Basis: decided here, after review finding 12.* The digest covers fields the
caller chose (`component.image`, toolset `name`, `mount-path`, interface
config), and the summary shows them. So the implementer shows its own catalog
name for a catalogued digest; otherwise it shows the image reference only
after confirming the repository serves that digest (and the tag only if it
resolves to it now), and refuses a reference it cannot confirm. Caller-chosen
names are shown as quoted identifiers limited to `[a-z0-9-_./:]`.

### Still not decided

- **Taint propagation** (#76's (g)): narrowing a child's egress as it reads,
  and narrowing a running instance's allowed hosts when an ancestor loses
  one (D8), both need runtime revocation of allowed hosts. Not in this
  version.
- **Records a guest builds** (`spawn-request`, `toolset-spec`, `grants`,
  `budget`) cannot gain fields after `@since`. Goals, swarms and a tree
  budget will want to extend them; decide before stabilizing whether they
  become resources or carry an extension field.

## Next: goals and swarms

Sketches, not WIT. Both build on spawn, and its shape leaves room for them:
children are addressed by durable session ids rather than in-process
handles, any host sharing the session store can act on them, results are
collected with `wait` rather than held in a stream, and progress is
contentless and host-authored.

### A goal interface: run until it is done

Today "keep going until X" is a prompt, which the model may abandon or
declare finished early. A goal makes it a first-class, host-checked loop:

- **The goal:** a statement for the agent, plus a **stopping condition** the
  host can evaluate without trusting the agent's word. Candidates: a JSON
  Schema the latest `structured` outcome must satisfy, a check run by a
  deterministic tool component (tests pass, a file exists), or a reviewer
  child whose structured verdict decides (an auditor the host installs).
- **Budget and limits:** a goal-level budget across runs (runs, tokens,
  wall-clock) on top of each run's `run-options`, and a cap on runs without
  progress.
- **The loop:** the host (or the implementer of a `goal` interface) sends the
  agent a run, evaluates the condition on its outcome, and sends the next run
  with the result of the check, until the condition holds, the budget ends,
  or a person stops it.
- **Progress:** contentless frames like `spawn-types.progress` plus a
  goal-level counter (runs, checks passed); the session's state stays the
  `session-status` one.
- **Shape:** probably `goal.start(spec) -> session id`, then `cancel` and
  `children` as for spawn, so a goal is a child session whose driver is the
  host rather than the parent's model. `wait` serves only runs the caller
  started, so a host-driven goal needs a session-level wait and a goal-level
  progress read, and rules for who owns the goal's runs. Both are new
  functions on a host-served interface, which is additive.
- **Slots:** a long-lived goal waiting on a person sits on a
  `concurrent-agents` slot; the limit may need to tell waiting from working.

### A swarm interface: membership and messages

Several agents working as peers rather than as parent and children:

- **Membership:** `register(role)`, `leave()`, `members()` within a swarm
  the host scopes (one per root session, say), each member a session id from
  spawn, so a swarm's members show in the same tree.
- **Messages:** `send(to, message)` and a per-agent inbox read with a stream,
  likely carried over the host's messaging (NATS on Control and Desktop), with
  the host stamping the sender's session id so a member cannot speak as
  another.
- **Bounds:** a member reaches only what its own grants allow; messages are
  content sent between agents, so the downward-flow rule applies to every
  hop, and a swarm's total reach is the union of its members', each admitted
  through spawn's ceiling rules. Inbox size and message rate limits keep a
  loop of agents from flooding each other.
- **Who may join:** sibling messaging is a new free-text channel between
  agents, so membership must be a power of its own that a `structured-only`
  child cannot hold, and a member's reach must rest on its ancestors as D8
  requires, so it cannot be re-used with a wider output.
- **What spawn already provides:** member creation and placement (spawn and
  `placement`), lifetime and the tree limits, state for each member, and the
  host's audit of who started whom.

## The approval digest

The digest is what coalescing, spawn tickets and the session ceiling
compare, and what a discovery offer's spec reproduces when loaded unchanged,
so two implementers must compute the same bytes.
`approval.get-request-digest` is the normative text.

- **Only what the person sees.** Instructions, labels and placement are not
  in it: free text the summary never shows, or a hint that never changes
  reach.
- **Normalized first.** Set lists are deduplicated and sorted, and hosts are
  normalized, so `API.example.com.` and `api.example.com` are one digest.
- **One JSON form for every WIT value,** serialized by RFC 8785.

**Test vector (load).** Caller workload incarnation
`0b6f1f9e-3c5d-4e2a-9a71-5d2c8e4f6a10`, component name `agent`, session id
`7d1e5c2a-90b4-4f3e-8a6d-2b9c0e1f4a37`, and a `toolset-spec` named `repo-fs`
for `ghcr.io/cosmonic/tools/fs:0.2.0` at `sha256:1c1c…1c` (the byte `1c` 32
times), with no `functions` and no `alias-prefix`, a read-only `workspace`
volume at `/src`, credential `github`, and allowed hosts given as
`API.GitHub.com.`, `api.github.com` and `https://Uploads.GitHub.com:443`.
The canonical JSON is:

```json
{"alias-prefix":null,"component":{"digest":"sha256:1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c","image":"ghcr.io/cosmonic/tools/fs:0.2.0"},"functions":null,"grants":{"allowed-hosts":["api.github.com","https://uploads.github.com"],"credentials":["github"],"interfaces":[],"secrets":[],"volumes":[{"mount-path":"/src","read-only":true,"volume":"workspace"}]},"name":"repo-fs"}
```

The preimage is `cosmonic:agent/toolset-spec@3`, a zero byte, the
incarnation, a zero byte, `agent`, a zero byte, the session id, a zero byte,
then those JSON bytes; the digest is
`sha256:c97b82787fe0b8a3209f9a8a9f32e16fd564bb3980a5a0fe5e7ecf6321c9db6b`.
Any reordering of hosts, volumes or credentials, any of the three host
spellings, or a duplicated entry must produce the same digest. (The tags
moved to `@3` because the preimage gained the session id and the records
gained fields.)

## Conformance cases

Not executable yet; every implementer of `spawn` or `tool-loading` should
pass them.

1. **Concurrent waiters.** N identical beyond-ceiling requests from one
   caller give one approval and one prompt. For a load, the grant adds one
   session ceiling entry, all N loads return the same toolset, and one
   rollout runs. For a spawn with N different keys, the ticket admits the
   first key's child; the others get one new approval, marked as a repeat.
   A resend with the admitted key returns the same child. The parent can
   load none of the spawn's approved toolsets without asking, and a second
   child with the same toolset and a wider output asks.
2. **Sessions apart.** The same request from the same component in two
   sessions gives two approvals with different digests; granting one admits
   nothing in the other.
3. **Session ceiling.** Grant a load. Set it aside and load again: `offered`,
   no prompt, no rollout. Unbind it at the component cap and load again: a
   rollout, no prompt. Have the person remove it, then load again:
   `denied` during the cooldown, then `needs-approval`. Remove it while a
   load of it is `loading`: the rollout is not published and the load
   answers `denied`; an unrelated rollout in flight still publishes. Remove
   it while a granted spawn ticket naming it is unconsumed: the resend asks
   again. Rebind its volume, publish a new digest, or change policy: each
   asks or refuses again.
4. **Same run.** Load a toolset mid-run and call it in the same run on the
   same instance. Fail the new component's start: `unavailable` and the
   toolsets are unchanged. Use a short `wait`: `loading`, then `offered`.
   On a host using the next-run fallback: `next-run` true, and the tools in
   the next run's `tools.list`. Hang a rollout: after the deadline the
   toolset leaves `loaded()` and an identical load starts again.
5. **Budget.** With the budget full, a load sets aside the least recently
   called unpinned toolset outside the residency window and lists it; with
   every offered toolset pinned, `over-limit(offered-functions)`.
6. **Children.** A child whose agent grants exceed the parent's own grants,
   or whose catalog definition does: `denied`. Remove a parent's entry a
   child's toolset rested on: the child loses it. Narrow the parent's own
   grants: the child's next `send` answers `denied` and its failure is
   `revoked`. Cancel a child: runs in progress in its whole subtree end
   `cancelled`. A child's toolset among the parent's set-aside tools: admitted.
   A grandchild requesting, as an agent grant, the reach of one of the
   child's toolsets: `denied`. Spawn past the depth limit:
   `over-limit(depth)`. `send` to a grandchild: `unknown`. `children(true)`
   lists the subtree parent-first. `end` on a child ends its subtree.
7. **Across stores and hosts.** Start a run with `send`, drop the stream and
   future, end the caller's store, and `wait` for its run id from a fresh
   store: the outcome arrives, and the run was not cancelled. Let a person
   start another run in the child first: `wait` still returns the caller's
   run. Place a child on another host and cut that host off mid-run: within
   the lease timeout, `wait` answers `host-lost`, the child's state is
   `failed` with `failure` `host-lost`, a later `send` starts a new run
   elsewhere, and writes or tool calls from the cut-off host are rejected.
   From a sibling component in the same session, `children` lists none of
   the first component's children and `send` answers `unknown`. With idle
   children at the concurrency limit, a further `send` answers
   `over-limit`.
8. **Structured only.** A child that returns valid structured output and puts
   text in its reply, its finish reason and its model name: the caller sees
   only the structured value and the implementer's own fields.
9. **Denial and cap.** After a denial, identical requests answer `denied`
   with no prompt until the cooldown ends. With the pending cap at K, a
   (K + 1)th distinct request answers `over-limit(pending-approvals)`.

## Tool naming: identity, alias, display

- **Identity carries authority.** Calls are routed, authorized and audited
  by the toolset component's identity and digest, never by an alias or a
  toolset name.
- **Aliases are the model's selection key.** A tool keeps its own name if no
  alias in the session has it; a later colliding tool is offered as
  `<toolset>__<tool>`. An alias is fixed for the session and stays reserved
  for its toolset name after the toolset leaves; the same name at a new
  digest takes it back, so a replaced toolset keeps its tools' names. A call
  always reaches the tool its model was offered: the host records which
  digest each alias denoted in the tool list it sent with a model call, and
  refuses a call whose alias now denotes another digest (`unknown-tool`).
  Definitions stay byte-stable except when a toolset is replaced. `alias-prefix` is the
  operator's override; a requested alias that is taken refuses the request
  rather than renaming.
- **Display is for people:** the tool's `title` and the toolset's name.

## Bounding tool results

A tool result stays in the agent's context and is resent on every later
model call, so the host bounds it: text over the cap is cut at a UTF-8
boundary with a note stating the original size, structured output over the
cap is dropped with a note, and an oversized attachment is replaced by a
note, as `tool-types.tool-response` states.

## The child's runs

A child is driven through the `run` export, the same one a host drives any
agent through, but the parent never sees its rich events. The implementer
mints the child's run ids, turns the lifetime budget into per-run options,
meters the child's model calls itself (it serves the child's `chat`), reads
the child's events promptly, emits only `working`, `blocked`, `cancelling`
and `counts`, replaces error details with its own, checks the outcome
against `output.schema`, `max-bytes` and `structured-only`, re-issues
attachments (D27), and keeps the outcomes of the runs the caller started,
keyed by session and run id, in the session store for `wait`.

## Considered and dropped

- **Confined spawn** (a child whose results go to a sink the caller cannot
  read): the use cases wanted privilege separation, which open spawn gives,
  or were better served by host-brokered credentials or a deterministic tool
  with a narrow return type.
- **A guest-side router or policy world:** a component in front of the
  implementer becomes the caller.

## Research grounding

- **Confinement and declassification.** Lampson, "A Note on the Confinement
  Problem" (1973); Myers and Liskov's decentralized label model (2000), which
  frames an approval as a declassification; Shapiro and Weber's EROS
  constructor (2000), which certifies that what it builds holds only listed
  capabilities plus approved "holes", as a `spawn-request` does.
- **Prompt injection patterns.** Willison's Dual LLM pattern (2023), CaMeL
  (Debenedetti et al., 2025), FIDES (Costa et al., 2025) and the design
  patterns paper (Beurer-Kellner et al., 2025) isolate untrusted content in
  a child with no tools and a constrained answer; `structured-only` supports
  that. None of them, nor these interfaces, claim to solve prompt injection
  or side channels.
- **How SDKs delegate.** The OpenAI Agents SDK's `Agent.as_tool()` and Claude
  Agent SDK subagents keep the parent in control and return only the final
  message; `spawn` keeps that shape and adds a sandbox and grants per tool, a
  ceiling the host checks, approvals bound to a digest, and durable child
  sessions. The Claude Agent SDK bounds the tree with depth and concurrency
  settings (D10).
- **Tool set size and caching.** Anthropic's tool search and
  mid-conversation tool change documentation, and OpenAI's function calling
  guide (D4, D6).

## What implementing it needs

- **A shared plugin.** `plugin-agent-spawn` serves `tool-discovery`,
  `tool-loading` and `spawn`, and works with the tools plugin that serves
  `tools`, so admission, the session ceiling and the budget are one
  implementation on Desktop and Control.
- **Rollouts a plugin can request.** On Control a workload's tools are its
  spec (`tool-components`, workload-owned and governed by admission), and a
  host plugin cannot write it. The plugin needs a control-plane path to
  request the revision, which admission policy checks like any other.
- **Retire after drain,** with the tool dispatcher routing to a toolset in
  the incoming revision meanwhile (D5).
- **A linearizable session store with parent links and leases.** Child
  sessions with their parent id, spawner identity, depth, labels, state and
  outcomes keyed by run id, readable by any host that serves the tree; run
  leases with fencing generations the store expires itself; and
  compare-and-swap for spawn tickets, idempotency keys and limit
  reservations (D7, D10, D12).
- **One session per workload,** with the session id in the per-workload
  bind configuration so plugins can attribute every call (D20).
- **Dispatch that never links toolsets,** including tool providers exporting
  `cosmonic:agent/tools` (D26). Control's session store (moving to SQLite, reconciled across
  zones) is the natural home; Desktop keeps the same records locally.
- **Session state for the ceiling and working set,** including the
  revocation generation, which toolsets are set aside or pinned, and the
  alias table, so a session that moves hosts keeps them.
- **An approval route.** Desktop has a person in the loop. Control has none
  yet; until it does, beyond-ceiling requests on Control are decided by
  policy alone.
- **Owned workloads.** Child workloads stop with their session and carry
  host-stamped owner fields a manifest cannot set.
- **Visibility into reach.** Comparable volume identity, the caller's secret
  and interface bindings, and an allowed-host containment check, exposed to
  plugins at bind.
- **Types-only interfaces linked once.** A component that imports a
  function-less interface only for its types needs it linked, and two plugins
  that both link it collide. The runtime should link function-less,
  resource-less interfaces itself, once per linker. `spawn-types` has a
  resource, so its provider exports it.

## Follow-ups

- The reference harness (`minimal-agent`) lists tools once per run; it
  should list again after a step that loaded tools (D6).
- Hosts should set a nonzero idle reclaim window on tool components (D4).
- `max-offered-functions` on the model binding and the workload manifest.

## Review

A Codex review (gpt-6-astra, extra-high reasoning) of the first revision
found nine issues; all are fixed above: sessions in caller identity (D20),
toolset reach kept as exact entries through every generation (D8), removal
as a revocation barrier (D2), no separate load ticket and ticket precedence
for spawns (D2), `structured-only` built from the implementer's own fields
(D16), provider worlds exporting `spawn-types` (D17), the loading state
machine's late failure, unbind and set-aside-while-loading cases (D4), store
teardown no longer cancelling a child's run (D7), and admission errors no
longer promising that no start-up code ran (D5). Two smaller fixes: the
README states the catalog condition for loads within the ceiling, and the
interface-grant example no longer names a WASI 0.2 interface.

A second round, on the hierarchical rework, found seven more; all are fixed:
children are bound to the full identity of the component that spawned them,
not only its session (D7); descendants' labels are no longer returned, since
a child wrote them (D7); runs hold a fenced lease so a cut-off host cannot
keep writing (D12); every run acceptance reserves its place against the
concurrency limit, with `start-error.over-limit` (D10); `send` returns a run
id and `wait` takes it (D7); a toolset still loading cannot be set aside
(D3); and the function budget is the smallest across the caller's model
aliases (D4).

### Third round (independent Claude review and a fresh Codex round)

Each finding was checked against the WIT first. All highs and mediums are
fixed; of the lows, all but one are fixed.

| # | Finding | Outcome |
| --- | --- | --- |
| 1 | A spawn approval grants more than its prompt says | Fixed: one child per decision via an idempotency key; nothing joins the parent's ceiling; the child's entries are bound to the spawn digest (D8) |
| 2 | "Never exceeds an ancestor" held only at admission | Fixed: provenance per entry, re-check at each run acceptance and child load, `revoked` (D8) |
| 3 | Remove did not void an unconsumed spawn ticket | Fixed: removal voids tickets naming the toolset; consumption re-checks everything (D2, D8) |
| 4 | Session attribution with several sessions per instance | Fixed: one session per workload, else `disabled`; chosen over a session context on the imports (D20) |
| 5 | Fencing does not stop a cut-off host's effects | Fixed: store-side expiry closes the generation, monotonic self-fencing with a margin, a linearizable store, and a plain list of what fencing does not stop (D12) |
| 6 | Toolsets could hold powers or be linked by siblings | Fixed: no `cosmonic:agent` interface grants; toolsets dispatched, never linked; linked tool providers refused (D26) |
| 7 | Catalog agents had no ceiling check | Fixed: definitions admitted by the component rules; aliases a child may delegate (D13) |
| 8 | Cancel and Stop did not reach the subtree | Fixed: `cancel`, `end` and a person's stop cascade (D7) |
| 9 | Session creation, prompts and spend unbounded | Fixed: `live-sessions` limit, subtree budgets, cooldown and pending cap per tree by reach, bounded retention (D10) |
| 10 | Placement could change network reach | Fixed: host-local names pin the child to the parent's host; placement audited (D11) |
| 11 | The revocation generation was session-wide | Fixed: the entry is the authority, the generation only detects races; refused steps answer `denied`; in-flight calls cancelled (D2) |
| 12 | Caller-chosen text reached the prompt | Fixed: catalog names or confirmed image references; quoted, limited identifiers (D29) |
| 13 | No fallback for same-run loading; `loading` had no end | Fixed: next-run fallback with `load-result.next-run`; rollout deadline (D5) |
| 14 | Fit rules underspecified | Fixed: volume access and sub-path rules, a host containment table (D28) |
| 15 | Attachments could not cross the boundary | Fixed: re-issued by the host, counted in `max-bytes`, none with `structured-only` (D27) |
| 16 | Notes said "keeps the last outcome" | Fixed ("The child's runs", "What implementing it needs") |
| 17 | Children orphaned when the spawner's identity ends | Fixed: they end with it (D7) |
| 18 | A prompt right after Remove | Fixed: removal starts the cooldown (D2) |
| 19 | Entries with nothing bound invisible | Fixed: the host UI lists every entry (D2) |
| 20 | Audit stated only for ceiling admissions | Fixed: every admission, refusal, removal, withdrawal, set-aside and unbind (`spawn-types`) |
| 21 | Reserved aliases blocked the same name at a new digest | Fixed: the same toolset name takes its aliases back (`tool-loading`) |
| 22 | Two providers give two `approval` types | Fixed: one implementer serves both (D17) |
| 23 | Guest-built records cannot grow; `placement.other` as input | `placement.other` is `invalid-request`. Record growth is declined for now and recorded under "Still not decided": cheap to change while unstable, and the right shape depends on goals and swarms |

### Fourth round (Codex re-review of the fixed head)

| # | Finding | Outcome |
| --- | --- | --- |
| 1 | Re-checks would withdraw a toolset approved for one child, since the parent's ceiling never held it | Fixed: the approval of the child is an authority source in re-checks (D8) |
| 2 | Self-fencing stopped model and tool calls but not direct effects | Fixed: the fence stops the child's instances, so nothing new starts through any import; only effects already under way remain (D12) |
| 3 | Subtree cancellation had no admission barrier | Fixed: a cancelling mark set atomically first, checked at every spawn and send acceptance in the subtree (D7) |
| 4 | Alias reclamation could retarget a call made against the old definitions | Fixed: calls are bound to the digest each alias denoted in the tool list sent with the model call (tool naming) |
| 5 | Idempotency keys kept for the session were unbounded | Fixed: `total-agents` defaults to 500 per tree, bounding key records (D10) |
| 6 | The rollout deadline could discard a revision staged for the next run | Fixed: the deadline covers preparation; a staged revision is kept until the next run or removal (D5) |
