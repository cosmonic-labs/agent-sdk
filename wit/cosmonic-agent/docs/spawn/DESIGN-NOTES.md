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
  the session id (D20).

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

### D2. Approvals feed a session-only ceiling, and removal is a barrier

*Basis: Eric, 2026-11-18 (session tool set design, Decisions; Cosmonic
Desktop implements the same rule as its session approval envelope). The
ticket and revocation details are decided here, after the Codex review.*

A person's approval of a toolset adds it to the **session ceiling**: entries
keyed to the caller, the toolset's request digest and its admission snapshot.
A later load of the identical toolset, after the host set it aside or
unbound it, after a rollback, or after a failed rollout was put back, is a
load within the ceiling and needs no prompt.

- **Loads have no separate ticket.** A granted load approval adds the entry
  at once, and any identical load is admitted through it. The "already held"
  rule keeps concurrent identical loads to one rollout, so nothing is
  multiplied. (#76's single-use grant for loads expired independently of the
  entry, which could admit a first load after the grant had expired.)
- **Spawns keep a one-time ticket,** consumed once across every waiter and
  resend, with expiry checked in the same step. A resend matching a pending
  or granted ticket is decided by the ticket before any ceiling check, so one
  decision never admits several children through coalescing. When the child
  is admitted, its beyond-ceiling toolsets join the parent's session ceiling.
  After that, an identical spawn fits the ceiling: the person approved reach
  for the session, and how many children use it is bounded by the tree
  limits. An operator who wants a prompt per child sets policy to ask; those
  approvals are marked as repeats.
- **Re-checked at each use,** in the step that admits it: the request is
  resolved again and compared with the entry's snapshot, and current policy
  is checked again. On Control, admission policy is cluster policy, so a
  session ceiling never becomes a way around a policy change made
  mid-session.
- **Never widens.** An entry admits its exact snapshot only.
- **Only the person removes, and removal is a barrier.** Removal drops the
  entry and advances the session's revocation generation. Every admission
  records its generation and is checked against it when a rollout
  publishes, when a set-aside toolset returns, and when a call is
  dispatched: a pending approval for the toolset resolves `expired`, a
  rollout adding it is not published, it is unbound, and an old revision
  cannot restore it. Descendant sessions holding it through that entry lose
  it the same way.
- **Session-scoped and audited.** It lives in the session's state, moves
  with the session, ends with it, and every admission through it names the
  approval it rests on.

*Why this fits single-use approvals.* Single use protected against N
coalesced requests riding one decision, reflex approval of a repeat, a name
rebound after review, and standing authority hidden in a reusable token. The
first three still hold (spawn tickets, repeat marking, snapshot comparison).
The fourth is answered as #76 already allowed: standing permission belongs
in policy a person wrote. An approval that adds to a session ceiling is
exactly that, a person writing "this session may hold this toolset at this
digest and reach", visible in the prompt ("Your answer lasts for this
session") and removable only by them.

*Alternatives.* Loads after `unload` as new requests that always ask
(defeats setting tools aside); treating a host-initiated unbind as "not an
unload" (bends what an admission means, no rule for rollbacks); a ceiling per
agent across sessions (an approval should not outlive the conversation it
was given in); keeping a single-use ticket for loads too (Codex review:
conflicts with the entry, as above).

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
   publishes only if the revocation generation is unchanged (D2).
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
| `spawn(request, input)` | Admits and creates a child session; with `input`, starts its first run. Returns the session id and the run's id. |
| `send(session, message)` | Starts a run in a direct child. Returns a host-minted run id, a progress stream and an outcome future. |
| `wait(session, run)` | The outcome of that exact run, from any store and host. The host keeps the outcome of every run the caller started until the child ends. |
| `cancel(session)` | Requests cancellation of the run in progress. |
| `end(session)` | Ends the child session and every session below it. |
| `children(descendants)` | Direct children, or the whole subtree in parent-first order, each with parent, depth, labels, `session-status` state, failure reason, location and whether it ended. |
| `get-lineage()` | The caller's own session id, parent and depth. |

- **Acting is on direct children only; seeing is on the subtree.** A parent
  sends, waits, cancels and ends its own children; it lists its
  descendants' state to draw its tree. The host records with each child the
  full identity of the caller that spawned it (workload incarnation,
  component and session) and acts only for that caller, so a sibling
  component bound to the same session neither sees nor drives another
  component's children. A session id authorizes nothing on its own.
- **Only the caller's own text comes back.** `children` returns labels only
  for direct children (the caller wrote them). A grandchild's labels were
  written by a child, and would let it pass text around its
  `structured-only` schema, so they stay in the host's view.
- **Runs are named.** `send` returns a host-minted run id and `wait` takes
  it, so a parent that reconnects after a person talked to the child still
  collects its own run's outcome, not the latest one.
- **States reuse `session-status`** (needs input, working, failed, done,
  idle), derived by the host. A child's reason for failing is the separate
  `child-failure` (host lost, start failed, budget exhausted), since
  `session-status` carries no reason.
- **Hosts show the tree** from their session store: the same parent links
  the interface reports. Desktop lists a child under its parent, open to step
  into and talk to; a CLI or Control's session API shows the same tree. A
  child's approvals reach the root session's person, labelled with the
  child's place in the tree.
- **Lifetime.** A child lives until `end`, its `max-duration`, or its parent
  session's end; ending cascades. A child with no run in progress holds no
  instance. Tree limits: `depth` 3 and `concurrent-agents` (children with a
  run in progress at once in one tree) 20 by default, always set (D10).
- **Explicit cancellation.** Dropping a progress stream or an outcome future
  only stops listening. Only `cancel` or `end` cancels. A child may run on
  another host and outlive the caller's store, and a caller that gets a
  fresh store per run must not cancel its children by finishing its own run.
  This differs from the `run` export, where dropping the stream cancels; that
  rule suits a driver that owns one in-process run, not a parent of durable
  sessions.

*Alternatives.* The #76 `agent` resource, store-scoped, ending on drop: a
child could not outlive one parent run, could not be shared with a later
store, and assumed the child was local. Detach and attach on that resource
(this PR's first revision): Codex found that a dropped stream still
cancelled an active run on store teardown and that `attach` could not
recover its outcome; session ids with `wait` remove both problems. Letting a
parent act on grandchildren: authority would skip a level, and the middle
agent could not reason about its own children.

### D8. A child's reach is bounded by every ancestor's ceiling

*Basis: Eric, 2026-11-18 (a child spawned with reach beyond its parent's
approval envelope never happens; the subagents milestone: a subagent's
grants are no wider than its parent's, enforced by the host); the mechanism
is decided here, with Codex's provenance fix.*

- The child agent component's own `grants` must fit the parent's own grants,
  or the spawn is `denied`. Reach the parent lacks goes in a toolset
  ("components follow reach").
- A child's toolset that fits the parent's ceiling (own grants by
  containment, or an exact toolset entry, so tools the parent set aside
  count) is admitted. One beyond it goes to a grantor only if it fits the
  ceiling of every ancestor above the parent (none, for a root parent);
  otherwise it is `denied`. The grant adds it to the parent's session
  ceiling when the child is admitted, so the child stays within its
  parent's ceiling.
- A child's toolsets become *toolset entries* in its own ceiling, never its
  own grants. So a child cannot give a grandchild's agent component the
  reach of one of its toolsets; it can only hand down that exact toolset.
- A child's loads follow the same rule: beyond its ceiling but within every
  ancestor's goes to the person, labelled with the child's place in the
  tree; beyond any ancestor's is `denied`.

*Alternatives.* The #76 shape, where a person could approve a child beyond
the parent: "no wider than the parent" became a default rather than an
invariant. Bounding by what the parent has *offered* rather than its whole
ceiling: setting a tool aside would shrink what a child may get. Treating a
child's toolsets as its own reach (this PR's first revision): Codex showed
this lends one approved toolset's reach to any component a child spawns.

### D9. Children may receive discovery and loading

*Basis: decided here.* `agent-powers` has `tool-discovery` and
`tool-loading` beside `spawn`. A child receives a power only when its request
asks for it and the parent holds it too. #76 withheld both because loading
had no bound; D8 gives it one. A parent still cannot load a toolset into a
child: it spawns a new child, so a child's tool set never changes under it by
someone else's hand.

### D10. Tree limits are always set

*Basis: decided here; answers #76's (y).* `depth` (default 3) and
`concurrent-agents` (default 20 per tree, counting runs from acceptance
until they have ended, cancelling included) are always set; the operator may change them. `total-agents` is
unset unless the operator sets it. These are the Claude Agent SDK's defaults
(`CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH` 3,
`CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS` 20). Budgets stay the caller's
choice and are not carved from the parent's, as in every SDK surveyed.
Every run acceptance (`send`, or `spawn` with input) reserves its place
atomically across the tree, and `send` answers `start-error.over-limit` when
it cannot, so idle children cannot all start at once past the limit.

### D11. Remote children: location-transparent, with a placement hint

*Basis: Eric, Bailey and Victor, 2026-11-19 (spawning subsessions for remote
sessions is the biggest gap); the shape is decided here.*

- **Nothing assumes the child is local.** Every operation names the child by
  session id and works through the host's session store, so a child may run
  on another host, host group or zone, and move between hosts between runs.
  Control keeps sessions in a store it reconciles across zones; Desktop may
  run everything locally behind the same interface.
- **Placement is a hint.** `spawn-request.placement` (`any`, `same-host`, a
  named `host-group`, a named `zone`) is honoured where the host can. It is
  not in the approval digest, because it never changes reach: a child runs
  only where its admission snapshot resolves identically (the same storage
  behind its volumes, the same credential bindings), and a hint that cannot
  be honoured without changing reach is ignored. `child.location` reports
  where it last ran, for display.
- **Streams are proxied.** `send` returns the same progress stream and
  outcome future whether the child is local or remote; the parent's host
  relays them from the session store.

*Alternatives.* A remote handle type separate from a local one: doubles the
surface and makes every harness branch. A mandatory placement: a hard
placement that cannot be met would fail spawns for reasons the agent cannot
see; policy is where hard residency rules belong.

### D12. A lost host fails the run, never hangs

*Basis: Eric, Bailey and Victor, 2026-11-19 (define what happens if a remote
child's host goes away); mechanism decided here, with fencing added after
the Codex review.* A child's run holds an exclusive lease in the session
store with a fencing generation (a counter the store advances for each new
lease) that every write, completion and tool dispatch for the run carries.
The host running it renews the lease, and stops starting model and tool
calls once it cannot. If the lease lapses (the operator sets the timeout;
recommended 60 seconds), the parent's host ends that run: waiters get `run-error.host-lost`, the child's state becomes
`failed`, and `child.failure` is `host-lost` naming the host. The store and
the tool dispatcher reject anything carrying the old generation, so a host
that was only cut off cannot write into the session or call tools for the
lost run once a new one starts. The child's session is durable, so a later
`send` starts a new run, placed again, under a new generation. A
store the parent cannot reach answers `unavailable`, never a silent wait.

*Alternatives.* Resuming the lost run elsewhere automatically: a run's
effects (tool calls with unknown outcomes) cannot safely be replayed, the
same reason the `run` contract records unresolved calls. Waiting
indefinitely for the host to return: the silent hang the direction rules
out.

### D13. A child's agent may be a catalog definition

*Basis: decided here.* `agent-source` is a pinned `component` with the
request's own model, grants and toolsets, or the name of an agent
`definition` in the operator's catalog, whose component, model, grants and
toolsets are the definition's. The admission snapshot resolves the name to
the definition's content, so a changed definition is a new request. This is
what "an agent definition or reference" needs without a second request
shape, and it answers #76's (l) (labeled imports for preset agents): a preset
is a catalog definition.

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
that imported it could not construct one.

### D18. Within the ceiling, only catalogued code loads without asking

*Basis: decided here; answers #76's (x).* A toolset loaded with the agent's
own hosts turns a latent grant into egress a model chooses. Within the
ceiling, the implementer admits at once only digests in the operator's
catalog or matching a toolset entry; any other digest goes to a grantor.

### D19. The package's error style

*Basis: decided here; answers #76's (j) and (aa).* `denied(string)` and
`other(string)`, as in `tool-types`, `run-types` and `session-status`. One
package, one style.

### D20. The session is part of caller identity

*Basis: decided here, after the Codex review.* `run` allows one component to
serve several sessions, so identity is the workload incarnation, the
component name *and* the session id. Approvals, coalescing, spawn tickets,
the cooldown, the cap and repeat tracking are partitioned by it, and the
digest preimage includes it. Without it, an identical request from session B
could coalesce onto, or consume, an approval shown to session A's person.

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

### Still not decided

- **Taint propagation** (#76's (g)): narrowing a child's egress as it reads
  needs runtime revocation of allowed hosts. Not in this version.

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
- **Shape:** probably `goal.start(spec) -> session id`, then the same
  `wait`, `cancel` and `children` as spawn, so a goal is a child session
  whose driver is the host rather than the parent's model.

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
   rollout runs. For a spawn, the ticket admits one child; the other resends
   are decided by the consumed ticket (a new approval, marked as a repeat)
   unless the ceiling now admits them without one, never several children on
   one ticket.
2. **Sessions apart.** The same request from the same component in two
   sessions gives two approvals with different digests; granting one admits
   nothing in the other.
3. **Session ceiling.** Grant a load. Set it aside and load again: `offered`,
   no prompt, no rollout. Unbind it at the component cap and load again: a
   rollout, no prompt. Have the person remove it, then load again:
   `needs-approval`. Remove it while a load of it is `loading`: the rollout
   is not published. Rebind its volume, publish a new digest, or change
   policy: each asks or refuses again.
4. **Same run.** Load a toolset mid-run and call it in the same run on the
   same instance. Fail the new component's start: `unavailable` and the
   toolsets are unchanged. Use a short `wait`: `loading`, then `offered`.
5. **Budget.** With the budget full, a load sets aside the least recently
   called unpinned toolset outside the residency window and lists it; with
   every offered toolset pinned, `over-limit(offered-functions)`.
6. **Children.** A child whose agent grants exceed the parent's own grants:
   `denied`. A child's toolset among the parent's set-aside tools: admitted.
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
  after its toolset leaves, so one alias never means two tools in one
  conversation and definitions stay byte-stable. `alias-prefix` is the
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
against `output.schema`, `max-bytes` and `structured-only`, and keeps the
last outcome in the session store for `wait`.

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
- **A session store with parent links and leases.** Child sessions with
  their parent id, depth, labels, last outcome and state, readable by any
  host that serves the tree, and a run lease that a lost host stops renewing
  (D11, D12). Control's session store (moving to SQLite, reconciled across
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
