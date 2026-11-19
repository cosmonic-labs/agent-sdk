# Spawn, tool discovery and tool loading: design notes

**Status: unstable, behind the `agent-spawn` feature.** The interfaces are
`spawn-types`, `spawn`, `tool-discovery` and `tool-loading` in
`cosmonic:agent` 0.4.4, with the worlds listed in [README.md](README.md).
They moved here from cosmonic-labs/awesome-cosmonic#76, where they were
drafted as a separate `cosmonic:agent@0.5.0` directory outside the build.
These notes record the rules, the decisions behind them, and what
implementing them needs. Each decision says what was chosen, why, and what
else was considered.

## What changed from awesome-cosmonic#76

- **Into the real package, at 0.4.4, unstable.** No second `cosmonic:agent`
  directory and no self-dependency on another version: the interfaces sit
  beside `tools` and `run` and use their types directly (D1).
- **Approvals feed a session ceiling.** A person's approval of a toolset adds
  it to the session's ceiling; the one-time grant is consumed by the first
  load, and later loads of the identical toolset are loads within the ceiling
  (D2).
- **`unload` sets aside.** It keeps the component and the approval and needs
  no rollout. Only a person removes a toolset from a session (D3).
- **Offered and ready states, and a function budget.** `toolset-state`,
  `load-result.set-aside`, `working-set`, and `limit-scope.offered-functions`
  (D4).
- **A load serves the run in progress.** `load` waits for the rollout, up to
  a deadline, and returns once the tools are callable; a pending approval
  returns at once. The old revision retires after the run ends (D5, D6).
- **Subagents on the same interface.** Children may receive discovery and
  loading; a child's reach is bounded by its parent's ceiling; children live
  in the parent's session and can be detached, attached and listed with
  their session state (D7 to D10).
- **Smaller shape changes:** `approval` is a resource (D15),
  `output-spec.structured-only` (D14), `toolset-spec.functions` (D12),
  `discovery-result.truncated` (D13), `progress.cancelling` (D16), the
  package's error style (`denied(string)`, `other(string)`) (D17), digest
  domain tags at `@3`.
- **Dropped from #76:** the separate package version and its `wkg.toml`
  override, the `harness` world (now `spawning-agent`, which includes
  `runnable-agent`), and the history of review rounds, which stays in #76.

## Information-flow rules

The WIT states these on `spawn-types`; here is why.

- **Reach** is what a request's grants can read and send data to. Read reach
  is the union of read grants (volumes, secrets, reading interfaces, the
  model alias) over the agent and its toolsets; egress reach is the union of
  allowed hosts and writing interfaces. A read-write volume counts both ways.
- **The ceiling** is what a caller may add without asking: its own reach,
  plus its session ceiling. Within it, results flowing up are not a leak,
  because the caller could have read the same data itself. Containment
  compares what a grant reaches, never its name: two workloads can call
  different storage `workspace`, so a volume fits only if the same storage
  backs one of the caller's mounts. A grant the implementer cannot see or
  compare is beyond the ceiling.
- **Beyond the ceiling is `needs-approval`.** An approval is an explicit
  declassification (deciding that data may cross a boundary it otherwise
  could not), bound to a digest over the caller and the request and to the
  admission snapshot of what the request's names resolved to. The caller may
  ask; only the host grants.
- **One approval, one admission.** Identical pending requests from one caller
  coalesce (one prompt), and the grant is consumed once across all of them.
  A grant expires, and expiry is checked in the step that consumes it.
- **An approval binds resolved reach, not names.** Names can be rebound. The
  snapshot holds the storage behind each volume, the credential binding
  behind each reference, the secret behind each name, the model behind the
  alias, each interface grant's resource and classification, every
  executable digest, and every default the request left open. Consuming the
  grant re-resolves and compares; a difference spends the grant and asks
  again. The person-facing UI shows the snapshot; the caller's summary never
  carries it.
- **No after a no.** After a denial the same request is refused without a
  prompt for a cooldown, and each caller has a capped number of pending
  approvals, so a looping agent cannot flood the person.
- **Downward flow.** Messages and instructions a caller sends a child can
  reach the child's egress. An approval for egress beyond the ceiling says
  so.
- **Plainly:** redaction is not a security boundary, because a model can
  paraphrase anything it read. A child's final reply is a declassification
  channel by nature. Its counts, finish code, error kind and timing are
  narrow channels it controls. Nothing here blocks prompt injection.
- **Caller identity** is the workload incarnation plus the component name,
  everywhere: digest, coalescing, cap, cooldown, session ceiling, loaded
  toolsets. A rollout keeps it, so a load never orphans the caller's pending
  approvals; a sibling component is a different caller and can never consume
  another's approval.

## Decisions

Each decision gives its basis: **Eric** where it follows a decision Eric
recorded (with the date), **Bailey** where it follows hers, and **decided
here** where this PR chose it after the research cited. Changing any of them
later is cheap while the interfaces are unstable.

### D1. Version and gating: 0.4.4, one feature, stacked on session-status

*Basis: decided here.* The interfaces are in `wit/cosmonic-agent` at 0.4.4,
the next patch after agent-sdk#3 (session-status, 0.4.3, unstable). Adding
interfaces and functions the host serves is additive, so a patch release is
right under the package's own versioning rules; nothing an existing agent or
tool was built against changes. This branch is stacked on #3 because `spawn`
reports a child's state with `session-status.status`, and so the two
versions cannot collide.

Every item carries `@unstable(feature = agent-spawn)`, the WIT-native way to
say "may change", and the same mechanism `agent-run` and `session-status`
use. Because the interfaces use `run-types` and `session-status`, a consumer
enables `agent-spawn`, `agent-run` and `session-status` together; with only
`agent-spawn`, `wasm-tools` refuses the gated `use`. The breaking 0.5.0 that
agent-sdk#4 calls for carries these interfaces unchanged.

*Alternatives.* A separate unpublished directory at 0.5.0, as #76 had: it
kept CI out but meant two versions of one package and a `wkg.toml` override
to build at all. One gate per power (`agent-spawn`, `tool-discovery`,
`tool-loading`), which WASI uses for unequally mature features: rejected
because all three share `spawn-types` and an item carries one gate, so a
per-power split would need a fourth gate for the shared types and every
consumer would enable it anyway.

### D2. Approvals feed a session-only ceiling

*Basis: Eric, 2026-11-18 (session tool set design, Decisions; Cosmonic
Desktop implements the same rule as its session approval envelope).*

A person's approval of a toolset adds it to the **session ceiling**: entries
keyed to the caller, the toolset's request digest and its admission snapshot
(executable digest, allowed hosts, the storage behind each volume and its
access, credential bindings, secrets, interface resources, materialized
defaults). The one-time grant on the approval is consumed by the first load,
exactly as before. A later load of the identical toolset, after the host set
it aside or unbound it, after a rollback, or after a failed rollout was put
back, is a load within the ceiling and needs no prompt.

- **Re-checked at each rebind,** in the step that admits it: the request is
  resolved again and compared with the entry's snapshot, and current policy
  is checked again. On Control, admission policy is cluster policy, so a
  session ceiling must never become a way around a policy change made
  mid-session.
- **Never widens.** An entry admits its exact snapshot, never something that
  merely fits inside it. Reusing an entry adds nothing.
- **Only the person removes.** Removing a toolset from the session (the
  person's action in the host's UI, never an import) drops its entry, so a
  later load asks again. A denial keeps its cooldown.
- **Session-scoped and audited.** It lives in the session's state, moves
  with the session, ends with it, and every admission through it names the
  approval it rests on. An implementer may bound the number of entries;
  dropping one only means the next load asks.

*Why this fits single-use grants.* Single use protects against four things:
N coalesced requests riding one decision, a reflex approval of a repeat, a
name rebound after review, and standing authority hidden in a reusable
token. The session ceiling keeps the first three intact (the grant is still
consumed once, a repeat is still marked, the snapshot is still compared) and
answers the fourth the way the #76 draft already allowed: standing
permission belongs in policy a person wrote. An approval that adds to a
session ceiling is exactly that, a person writing "this session may hold
this toolset at this digest and reach", scoped to the session, visible in
the approval prompt ("Your answer lasts for this session"), and removable
only by them.

*Alternatives.* (a) Keep loads after `unload` as new requests that always
ask: correct but defeats setting tools aside, since every return would
prompt. (b) Treat a host-initiated unbind as "not an unload", so the
original admission stands: weaker, because it bends what an admission means
and gives no rule for a rollback. (c) A ceiling per agent across sessions:
rejected, since an approval should not outlive the conversation the person
gave it in.

### D3. `unload` sets aside; only a person removes

*Basis: Eric, 2026-11-18.* `unload(name)` moves a toolset the caller loaded
to `ready`: its definitions leave the model's tool list, its component and
its approval stay, and no rollout runs. Loading it again returns it at once.
Nothing in these interfaces removes a toolset from a session: that is the
person's Remove, which also drops its session ceiling entry. Pinned and
declared toolsets cannot be set aside by the caller (`denied`).

*Why.* A harness unloads to keep the model's list sharp, not to give up
authority, and a set-aside costs nothing to undo. Removal is a decision
about reach and belongs to the person who granted it.

*Alternatives.* `unload` as a rollout that unbinds (the #76 shape): it costs
a rollout each way and re-approval on return, for no gain in safety. A
model-facing unload tool: not in this version; models rarely clean up
unprompted, and the host's budget does it for them.

### D4. Offered and ready states, and a per-model function budget

*Basis: Eric, 2026-11-18 (session tool set design and its five decisions).*

A toolset in a session is **approved** (in the session ceiling), **bound**
(its component is in the workload) and **offered** (its definitions are in
the model's prompt), with one limit each. The WIT reports what a caller can
act on: `toolset-state` is `loading`, `offered` or `ready` (bound and
approved, not offered). `loaded()` returns a `working-set` with every
toolset (declared ones too, so a harness can count the budget),
`offered-functions` and `max-offered-functions`.

- **The budget** comes from the model binding (`max-offered-functions` on
  the binding, defaulting from the provider family and capped by the
  provider's hard limit, 128 on OpenAI Chat Completions); the workload
  manifest may only lower it. The host's own discovery tools count.
  Recommended defaults: 32 for hosted frontier models, 20 for an unknown
  model, 10 for local models up to about 14B parameters.
- **Which model.** The budget of the model the caller's session last
  called; before any call, the smallest budget among the aliases the caller
  is granted. *Decided here*, matching what Desktop does.
- **Set-aside on load.** When a load would go over the budget, the
  implementer sets aside the least recently called toolset the caller
  loaded (never called before called, then the oldest load), never a pinned
  one and never one loaded or called within the residency window
  (recommended three runs). The load's result lists what it set aside, so a
  harness knows its list changed. Nothing is set aside on a timer.
- **Unbinding** happens only at the component cap (`loaded-toolsets`,
  recommended 16 per agent workload), only to set-aside unpinned toolsets,
  inside a load's own rollout, and never with a call in flight. An idle bound
  tool costs little (compiled code is shared by digest, and a tool with a
  nonzero idle reclaim window holds no instance), so the cap is a prompt and
  audit limit more than a memory one.
- **`over-limit(offered-functions)`** when nothing can make room.

*Why a function count.* Selection accuracy falls as the list grows
(Anthropic: selection "degrades once you exceed 30 to 50 available tools";
OpenAI: "aim for fewer than 20 functions available at the start of a
turn"), small local models need a much smaller list, and providers have hard
caps. A count tracks that better than a cap on components, which can hold
one function or ten.

*Alternatives.* A hard cap on components (simple, but blind to the model and
to component size); a token budget for definitions (tracks cost, but
tokenizers differ by about two times and people cannot reason about it; log
it, enforce later if count proves too coarse); provider tool search with
deferred loading (cache-friendly, but provider-specific and unavailable for
local models; adapters may still use it for approved tools only).

### D5. A load serves the run in progress

*Basis: goal from Eric, 2026-11-19 (spawn makes loading a shared interface
that serves the run in progress); mechanism decided here.*

Cosmonic Desktop today rolls the session's workload out when a tool is
loaded, and the new revision serves the agent's next run, which Desktop
starts at once so the turn reads as continuous. The goal of moving loading
into the SDK is that a load serves the run that asked for it.

**How.** Tools are linked to nothing: the agent calls them through its
`tools` import, which the host serves, and the host dispatches each call to
the toolset's component by its identity. So the agent's instance does not
need to change for a new toolset to be callable.

1. `load` admits the request (D2's ceiling, the catalog rule in D19, or
   `needs-approval`).
2. If the toolset is not bound, the implementer rolls the workload out to a
   revision that adds its component. Every new component must start and pass
   its checks; otherwise `load` answers `unavailable` and nothing changes.
3. The run in progress is not moved. The caller's instance keeps running it
   in the old revision, and the session's tool dispatcher routes calls to the
   new toolset in the new revision. The old revision retires once the run in
   progress ends (a drain), and the next run starts on the new revision.
4. The toolset becomes `offered` and `load` resolves.

**The deadline.** `load(spec, wait)` resolves when the toolset is callable,
or when `wait` runs out, whichever is first. `wait` is capped by the
implementer's own limit (recommended 30 seconds, about the time Desktop
allows an image pull) and by what the caller's run has left of its
`max-duration`. When it runs out, `load` returns the toolset as `loading`;
the rollout continues, `loaded()` shows when it is `offered`, and an
identical `load` joins it.

**Pending approval never holds the run.** Beyond the ceiling, `load`
answers `needs-approval` at once with an `approval` handle. The harness
chooses: finish the run (`input-required`) and let the person's answer
start the next one, or keep working and resend when `wait-for-decision`
resolves `granted`. A held approval also shows as `needs-input` in the
session's state.

*Alternatives.* (a) Keep "next run" semantics and have the host start the
next run at once (Desktop today): simple, but the agent loses its in-memory
plan and the model sees a seam. (b) Replace the agent's instance mid-run and
resume from the journal: the `run` contract says an agent must not rely on
memory surviving between runs, not within one, so this would break harnesses.
(c) Run each loaded toolset as its own workload until the next rollout, as
the first spike did: it splits a toolset's identity across two homes and
contradicts "a tool is a component in the agent's workload". (d) Block in
`load` until the person decides: holds the turn indefinitely.

### D6. How a harness learns new tool definitions mid-run

*Basis: decided here.* No new event or stream. Once a toolset is `offered`
the caller's next `tools.list` includes its tools; once `ready`, it leaves
them out. A harness that loads tools, or lets its model call the host's
`load_tools`, calls `tools.list` again before its next model call (the
reference harness lists once per run today and will list again after a
step that loaded tools). The host serves `list` from its own table without
running guest code, so re-listing is cheap.

The prompt cache is the host's concern, not the harness's. Changing the
tool list at the front of the prompt invalidates a provider's cache, and on
models with preserved reasoning it can invalidate earlier reasoning too.
Anthropic's mid-conversation tool changes (a `role: "system"` message with
`tool_addition` and `tool_removal` blocks) keep the cached prefix and the
earlier reasoning valid; Cosmonic Desktop's Anthropic adapter already sends
a session's tool changes that way, diffing the list the agent sends against
the one the conversation started with. On OpenAI, newly loaded tools
appended at the end keep the cache up to that point. Aliases never change
within a session, which keeps definitions byte-stable.

*Alternatives.* A `tools-changed` stream on `tool-loading`: one more thing to
wire up, and it would miss changes made through the model-facing
`load_tools`. Returning the new definitions from `load`: the harness would
merge lists itself and could drift from what the host dispatches.

### D7. A child's reach is bounded by its parent's ceiling

*Basis: Eric, 2026-11-18 (a child spawned with reach beyond its parent's
approval envelope never happens; the subagents milestone: a subagent's
grants are no wider than its parent's, enforced by the host); the mechanism is decided here.*

- The child agent component's own `grants` must fit the parent's own reach,
  or the spawn is `denied`. Reach the parent lacks belongs in a toolset
  ("components follow reach").
- A child's toolset may fit the parent's ceiling (own reach, or an exact
  session ceiling entry, so tools the parent set aside count, since they are
  approved). One beyond it goes to a grantor; the person's approval adds it
  to the **parent's** session ceiling, so the child is still within its
  parent's ceiling after the grant.
- A child's own ceiling is its spawned reach plus its own session ceiling. A
  child's load beyond its own ceiling but within its parent's goes to the
  person, labelled with the child's name; one beyond the parent's ceiling is
  `denied`.

*Alternatives.* The #76 shape, where a person could approve a child beyond
the parent: it made "no wider than the parent" a default rather than an
invariant. Bounding the child by what the parent has *offered* rather than
its whole ceiling: rejected, because set-aside tools are approved and
setting one aside should not shrink what a child may get.

### D8. Children may receive discovery and loading

*Basis: decided here.* `agent-powers` gains `tool-discovery` and
`tool-loading` beside `spawn`. A child receives a power only when its
request asks for it and the parent holds it too. #76 withheld both from
children because loading had no bound; D7 gives it one, and a child is a
full agent with the same run model as any other. A parent still cannot load
a toolset into a child it spawned: it spawns a new child, so a child's tool
set never changes under it by someone else's hand.

### D9. Children live in the parent's session; detach and attach

*Basis: decided here; answers #76's questions (c) and (s).* A child belongs
to the caller's session, not to the store that spawned it. Dropping an
attached handle ends the child (the safe default: a trap or a forgotten
handle cannot leak children). `agent.detach` releases the handle and keeps
the child; `spawn.attach(id)` takes it back in a later run. The id is
host-minted and authorizes nothing on its own: `attach` checks the caller's
identity and session. At most one handle to a child exists at a time.
Detached children count toward the tree limits and end after an idle period
the operator sets (recommended 15 minutes without a run), when their budget
runs out, or with the session.

*Why.* Most callers get a fresh store per run, so store-scoped handles
meant a child could never outlive one run, and a parent working across
several runs (a research child it consults twice) had to spawn again and
raise a repeat approval.

*Alternatives.* Store scope only (the #76 default): too short for the common
case. A label the guest chooses as the reattach key: a guest could leak or
guess it, so authority stays with the host-derived caller identity.

### D10. Child sessions are visible to the parent

*Basis: the subagents milestone (a child session listed under its parent, open to step into,
its approvals reaching the parent's person labelled with the child's name);
the WIT shape is decided here.* `spawn.children()` returns each living child:
its id, its labels (the parent's own text), whether it is attached, and its
session state from `session-status` (needs input, working, failed, done,
idle), which the host derives from its own facts. The parent sees state, not
content: the child's conversation stays in the host's own view, where a
person can open it and talk to the child.

### D11. Tree limits are always set

*Basis: decided here; answers #76's (y).* `depth` (default 3) and
`concurrent-agents` (default 20, per spawn tree) are always set, and the
operator may raise or lower them; `total-agents` stays unset unless the
operator sets it. These are the Claude Agent SDK's defaults
(`CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH` 3, `CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS`
20). #76 left every tree limit unset by default, so turning spawn on allowed
an unbounded tree. Budgets stay the caller's choice and are not carved from
the parent's (as in every SDK surveyed); cost is bounded by the tree limits,
per-host budget caps, and provider spend limits.

### D12. `toolset-spec.functions` trims a toolset

*Basis: decided here.* An optional list of the functions to offer, by their
own names; none offers all. It is part of the digest. Large components spend
the function budget on functions a job never uses; Cosmonic Control's tools
plugin already has the same idea as its `tool-functions` key, so a manifest
and a spawn request say the same thing.

### D13. Discovery says when it truncated

*Basis: decided here.* `list` returns a `discovery-result` with `truncated`.
#76 noted that a capped list without a signal could not be fixed later
without breaking callers; while the interface is unstable it costs nothing.

### D14. `output-spec.structured-only`

*Basis: decided here; answers #76's (n), in part.* True makes the host empty
`reply` before the caller sees it and requires a schema. It lets the host,
not harness code, enforce the quarantine pattern, and lets an approval
summary say that only structured data returns, a much smaller
declassification. The host does not check that the schema is closed (no free
strings); that check is a possible later addition. Opaque references a parent
can pass on but not read (as in CaMeL and FIDES) are not added: they need
host-side value storage and provenance that nothing else here has.

### D15. `approval` is a resource

*Basis: decided here; answers #76's (w).* A record holding a `future` could
never gain a field without breaking; a resource with `get-` accessors and
`wait-for-decision` can gain methods additively. It stays inside
`admission-error.needs-approval`. Dropping the handle only stops waiting.

### D16. Cancellation matches the `run` export, with a `cancelling` frame

*Basis: decided here; answers #76's (u).* Dropping the progress stream
requests cancellation, as dropping the event stream does for `run`, so one
package has one rule. #76's recommendation was to key on dropping the outcome
future instead; it would differ from `run` for no gain once harnesses follow
`run`'s documented rule. `progress.cancelling` covers the window in which
`agent.run` answers `busy` because a cancelled run has not stopped.

### D17. The package's error style

*Basis: decided here; answers #76's (j) and (aa).* `denied(string)` and
`other(string)`, as in `tool-types`, `run-types` and `session-status`, rather
than the design skill's `access-denied` and `other(option<string>)`. One
package, one style.

### D18. Discovery shows what could be granted; providers start off

*Basis: #76 defaults 3 and 6, kept.* `list` returns offers within the
ceiling and offers beyond it that policy could approve, each with its fit,
and hides what policy would never grant. Each power (discovery, loading,
spawn) is off until the operator enables it; a power that is off answers the
typed `disabled` and is audited, and the workload still starts, so an agent
built against `spawning-agent` runs with whatever powers it is allowed.

### D19. Within the ceiling, only catalogued code loads without asking

*Basis: decided here; answers #76's (x).* Grants are reach, not effective
reach: a toolset loaded with the agent's own hosts turns a latent grant into
egress a model chooses. Within the ceiling, the implementer admits at once
only digests in the operator's catalog or in the session ceiling; any other
digest goes to a grantor. This matches Desktop, where loads come only from
the catalog.

### D20. The WIT interfaces and the host's discovery tools are one mechanism

*Basis: decided here.* The package reserves `find_tools` and `load_tools`
for a host's own model-facing discovery tools. `tool-discovery` and
`tool-loading` are the harness-facing form of the same thing: one catalog,
one admission, one session ceiling, one budget. A harness can use either or
both; a load through either shows in the other's `loaded()` and the same
session ceiling.

### D21. Annotations never feed admission

*Basis: Eric, 2026-11-18 (timeouts decision), and agent-sdk#4.* Reach comes
from grants, never from a tool's `read-only` or other annotations, which are
publisher claims. How hosts check `read-only` against a tool's grants before
passing it through, and what a harness may do after a timeout on a read-only
tool, is agent-sdk#4's change to `tool-types`; these notes do not repeat it.

### D22. The toolset lives in the agent's workload

*Basis: Bailey, 2026-10-16 (tools are components with the same boundary as
any other; nothing is special); answers #76's (a).* A toolset joins the
agent's own workload as a component, and loading is a rollout attached to
the same session.

### D23. Where it is implemented

*Basis: Eric, 2026-11-18.* The interfaces live here; the implementation is
one shared host plugin, `plugin-agent-spawn`, in Cosmonic Control, which
Cosmonic Desktop uses the way it uses Control's typed tool crate. See
"What implementing it needs".

### Still not decided

- **Taint propagation** (#76's (g)): narrowing a child's egress as it reads
  needs runtime revocation of allowed hosts. Not in this version.
- **Labeled imports for preset agents** (#76's (l)): not added. A preset is a
  catalog entry whose spec a harness passes to `spawn`; nothing needs a
  second import shape yet.

## The approval digest

The digest is what coalescing, single use and the session ceiling compare,
and what a discovery offer's spec reproduces when loaded unchanged, so two
implementers must compute the same bytes. `approval.get-request-digest` is
the normative text.

- **Only what the person sees.** The digest covers exactly the fields the
  summary shows. Instructions and labels are caller free text the summary
  never shows; a digest over them would let a caller raise prompts that
  differ in nothing visible.
- **Normalized first.** Set lists are deduplicated and sorted, and hosts are
  lowercased, stripped of one trailing dot, converted to A-labels, and lose a
  default port, so `API.example.com.` and `api.example.com` are one digest.
  The same normal form feeds the ceiling check.
- **One JSON form for every WIT value,** serialized by RFC 8785.

**Test vector (load).** Caller workload incarnation
`0b6f1f9e-3c5d-4e2a-9a71-5d2c8e4f6a10`, component name `agent`, and a
`toolset-spec` named `repo-fs` for `ghcr.io/cosmonic/tools/fs:0.2.0` at
`sha256:1c1c…1c` (the byte `1c` 32 times), with no `functions` and no
`alias-prefix`, a read-only `workspace` volume at `/src`, credential
`github`, and allowed hosts given as `API.GitHub.com.`, `api.github.com` and
`https://Uploads.GitHub.com:443`. The canonical JSON is:

```json
{"alias-prefix":null,"component":{"digest":"sha256:1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c","image":"ghcr.io/cosmonic/tools/fs:0.2.0"},"functions":null,"grants":{"allowed-hosts":["api.github.com","https://uploads.github.com"],"credentials":["github"],"interfaces":[],"secrets":[],"volumes":[{"mount-path":"/src","read-only":true,"volume":"workspace"}]},"name":"repo-fs"}
```

The preimage is `cosmonic:agent/toolset-spec@3`, a zero byte, the
incarnation, a zero byte, `agent`, a zero byte, then those JSON bytes; the
digest is
`sha256:8b81d74e1b1d94452acec3cce9cd3be85731a0bd9b8489ea519a1ff1c7637cc4`.
Any reordering of hosts, volumes or credentials, any of the three host
spellings, or a duplicated entry must produce the same digest. (The tags
moved from `@2` to `@3` because `toolset-spec` gained `functions` and
`spawn-request` gained `structured-only` and two powers.)

## Conformance cases

Not executable yet; every implementer of `spawn` or `tool-loading` should
pass them.

1. **Concurrent waiters.** N identical beyond-ceiling requests from one
   caller give one approval (same id and digest) and one prompt; one grant
   admits one child or one load. For a load, all N resends return the same
   toolset and one rollout runs; for a spawn, one child starts and the
   others get one new approval marked as a repeat.
2. **Session ceiling.** Load a toolset beyond the ceiling and grant it. Set
   it aside and load it again: `offered`, no prompt, no rollout. Force an
   unbind at the component cap and load it again: a rollout, no prompt. Have
   the person remove it and load it again: `needs-approval`. Rebind its
   volume to other storage, or publish a new digest, and load it again:
   `needs-approval`. Change policy to refuse it and load it again: `denied`.
3. **Same run.** Load a toolset within the ceiling mid-run, then call one of
   its tools in the same run: the call succeeds, and the run's instance is
   the one that started it. Fail the new component's start: `unavailable`,
   and the caller's toolsets are unchanged. Use a `wait` shorter than the
   rollout: `loading`, then `offered` in `loaded()`.
4. **Budget.** With a budget of N and N functions offered, load another:
   the least recently called unpinned toolset outside the residency window is
   set aside and listed in `set-aside`. With every offered toolset pinned:
   `over-limit(offered-functions)`.
5. **Children.** A child whose agent grants exceed the parent's reach:
   `denied`. A child's toolset in the parent's set-aside tools: admitted. A
   child's load beyond its parent's ceiling: `denied`. Spawn past the depth
   limit: `over-limit(depth)`. Detach, end the run, attach from a fresh
   store: the same child, its conversation and budget intact; attach from a
   sibling component: `unknown`.
6. **Normalization and snapshot.** Requests differing only in set order,
   duplicates, host spelling, labels or instructions share one approval; a
   rebinding while pending withdraws it (`expired`, no cooldown) and raises
   one new one; a rebinding after the grant spends the grant.
7. **Denial and cap.** After a denial, identical requests answer `denied`
   with no prompt until the cooldown ends. With the pending cap at K, a
   (K + 1)th distinct request answers `over-limit(pending-approvals)`.

## Tool naming: identity, alias, display

- **Identity carries authority.** Calls are routed, authorized and audited
  by the toolset component's identity and digest. Neither an alias nor a
  toolset name ever feeds a routing or grant decision.
- **Aliases are the model's selection key** and carry no authority. A tool
  keeps its own name if no alias in the session has it; a later colliding
  tool is offered as `<toolset>__<tool>`. An alias, once assigned, is fixed
  for the session and stays reserved after its toolset leaves, so one alias
  never means two tools in one conversation and definitions stay
  byte-stable. Declared toolsets are assigned in declaration order.
  `alias-prefix` is the operator's override; a requested alias that is taken
  refuses the request rather than renaming.
- **Display is for people:** the tool's `title` and the toolset's name,
  never the wire alias.

## Bounding tool results

A tool result stays in the agent's context and is resent on every later
model call, so the host bounds it, whatever the toolset does: text over the
cap is cut at a UTF-8 boundary with a note stating the original size,
structured output over the cap is dropped with a note, and an oversized
attachment is replaced by a note. `tool-types.tool-response` states the
rule; the caps are the host's.

## The child's runs

A child is driven through the `run` export, the same one a host drives any
agent through, but the parent never sees its rich events. The implementer
mints the child's run ids, turns the lifetime budget into per-run options,
meters the child's model calls itself (it serves the child's `chat`), reads
the child's events promptly so neither side blocks the other, emits only
`working`, `blocked`, `cancelling` and `counts`, replaces error details with
its own, and checks the outcome against `output.schema`, `max-bytes` and
`structured-only`.

## Considered and dropped

- **Confined spawn** (a child whose results go to a sink the caller cannot
  read): the use cases wanted privilege separation, which open spawn gives,
  or were better served by host-brokered credentials or a deterministic tool
  with a narrow return type. Reopen it if a use case appears that none of
  these covers.
- **A guest-side router or policy world:** a component in front of the
  implementer becomes the caller.

## Research grounding

- **Confinement and declassification.** Lampson, "A Note on the Confinement
  Problem" (1973), names legitimate, storage and covert channels; a
  quarantined child's schema is the legitimate channel, and its finish code
  and timing are covert ones. Myers and Liskov's decentralized label model
  (2000) frames an approval as a declassification; here the host is the one
  principal that decides it. Shapiro and Weber's EROS constructor (2000)
  certifies that what it builds holds only listed capabilities plus approved
  "holes"; a `spawn-request` is that constructor.
- **Prompt injection patterns.** Willison's Dual LLM pattern (2023), CaMeL
  (Debenedetti et al., 2025), FIDES (Costa et al., 2025) and the design
  patterns paper (Beurer-Kellner et al., 2025) all isolate untrusted content
  in a child with no tools and a constrained answer. `structured-only` with
  a closed schema supports that; none of these papers, nor these
  interfaces, claim to solve prompt injection or side channels.
- **How SDKs delegate.** The OpenAI Agents SDK's `Agent.as_tool()` and Claude
  Agent SDK subagents keep the parent in control and return only the final
  message; `spawn` keeps that shape and adds a sandbox and grants per tool,
  a ceiling the host checks, and approvals bound to a digest. Neither carves
  a child's budget from its parent's; the Claude Agent SDK bounds the tree
  with depth and concurrency settings (D11).
- **Tool set size.** Anthropic's tool search documentation and OpenAI's
  function calling guide (D4); Anthropic's mid-conversation tool changes and
  prompt caching documentation (D6).

## What implementing it needs

From Cosmonic Desktop's discovery and session tool set work, Control's tools
plugin, and wash-runtime:

- **A shared plugin.** `plugin-agent-spawn` serves `tool-discovery`,
  `tool-loading` and `spawn`, and works with the tools plugin that serves
  `tools`, so admission, the session ceiling and the budget are one
  implementation on both Desktop and Control.
- **Rollouts a plugin can request.** On Control, a workload's tools are its
  spec (`tool-components`, workload-owned and governed by admission), so a
  load is a new revision of that spec, and a host plugin cannot write it. The
  plugin needs a control-plane path to request the revision, which admission
  policy then checks like any other.
- **Retire after drain.** The old revision retires only once the run in
  progress ends, and the tool dispatcher routes calls to a toolset in the
  incoming revision during that window (D5).
- **Session state for the ceiling and working set.** The session ceiling,
  which toolsets are set aside or pinned, and the alias table live in the
  session's state, so a session that moves hosts keeps them.
- **An approval route.** Desktop has a person in the loop. Control has no
  person-facing approval yet; until it does, beyond-ceiling requests on
  Control are decided by policy alone, and `needs-approval` is returned only
  where a route exists.
- **Owned workloads.** Children are workloads that stop with their parent's
  session and carry host-stamped owner fields a manifest cannot set.
- **Visibility into reach.** Comparable volume identity (the storage behind
  a mount, not its name), the caller's secret and interface bindings, and an
  allowed-host containment check, exposed to plugins at bind.
- **Types-only interfaces linked once.** A component that imports a
  function-less interface (`spawn-types`, `tool-types`) only for its types
  needs it linked, and two plugins that both link it collide. The runtime
  should link function-less, resource-less interfaces itself, once per
  linker.
- **Metering.** The implementer serves the child's `chat`, so it counts
  tokens itself and enforces `max-total-tokens` mid-run.

## Follow-ups

- The reference harness (`minimal-agent`) lists tools once per run; it
  should list again after a step that loaded tools (D6).
- Hosts should set a nonzero idle reclaim window on tool components, so a
  bound tool nobody calls holds no instance (D4).
- `max-offered-functions` on the model binding and the workload manifest.
