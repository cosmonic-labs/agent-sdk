# Spawn, tool discovery and tool loading

**Status: unstable.** These interfaces are part of `cosmonic:agent` 0.4.4
behind the `agent-spawn` feature. They use types from `run-types` (feature
`agent-run`) and `session-status` (feature `session-status`), so a consumer
enables all three: wit-bindgen takes `features: ["agent-spawn", "agent-run",
"session-status"]`, and a Wasmtime host sets the same options on the
`LinkOptions` its generated `add_to_linker` takes. The shape may change before
the items are marked `@since`. Why each rule is the way it is, and the
decisions behind it, are in [DESIGN-NOTES.md](DESIGN-NOTES.md).

An agent with a job often needs help it did not start with: a tool it finds
it needs halfway through a task, or a second agent to review one file or to
read content it should not trust. These interfaces give an agent three
powers, each its own import, so a workload gets exactly the ones its world
names:

- **tool-discovery**: ask which toolsets are available to it;
- **tool-loading**: load a toolset into itself while it runs, and set one
  aside;
- **spawn**: start child agent sessions, send them input, wait for their
  results, cancel and end them, and see the tree they form.

A **toolset** is one component, with its own grants (allowed hosts, volumes,
interfaces, credential references), that offers one or more tools. It is
either a tool provider (it exports `cosmonic:agent/tools`) or a typed tool
(it exports its own interfaces, and the host offers their functions as
tools). "Tool" means one definition or call, as in `tools`.

The host is the only grantor. An agent can ask for more than it has, but only
the host, by policy, an auditor it installed, or a person, can approve it.
Credentials never pass through these interfaces: models, secrets and
credentials are named, and the host resolves them.

## Vocabulary

| Term | Meaning here |
| --- | --- |
| reach | What a request's grants can read (read reach) and send data to (egress reach). |
| ceiling | What a caller may add without asking: its own reach, plus its session ceiling. |
| session ceiling | The toolsets a person approved in this session, each at the exact digest and resolved reach they reviewed. It lasts as long as the session, and only the person removes an entry. |
| offered | A toolset whose definitions are in the model's tool list. |
| ready | A toolset that is bound and approved but set aside, out of the model's list to stay within its function budget. Loading it again takes no prompt and no rollout. |
| function budget | How many functions a model is offered at once, from its model binding (`max-offered-functions`). |
| rollout | Moving a workload to a new revision, here one that adds a toolset's component, attached to the same session. |
| run | One unit of work: a message in, the agent's loop of model and tool calls, a reply out. |
| step | One model call inside a run. |
| caller | The component an import is bound to, in one session: its workload incarnation, component name and session id. |
| child session | An agent session started by `spawn`, with its own workload, conversation and state, recorded under its parent session. |

## Worlds

| World | Imports | Exports | Who targets it |
| --- | --- | --- | --- |
| `tool-discovery-client` | `tool-discovery` | none | Included by an agent that may browse the catalog |
| `tool-loading-client` | `tool-loading` | none | Included by an agent that may change its own toolsets |
| `spawn-client` | `spawn` | none | Included by an agent that may start child sessions |
| `spawning-agent` | `runnable-agent` and all three | `run` | A runnable agent with every power |
| `tool-discovery-provider` | none | `tool-discovery` | An implementer, or a test double |
| `tool-loading-provider` | none | `spawn-types`, `tool-loading`, `tools` | An implementer, which must also serve the caller's `tools` |
| `spawn-provider` | none | `spawn-types`, `spawn` | An implementer, or a test double |

A child agent targets `runnable-agent`, the same world a host drives any agent
through. Each client world holds only its own import, so an agent composes
the powers it needs:

```wit
package acme:coder@1.0.0;

world coder {
    include cosmonic:agent/runnable-agent@0.4.4;
    include cosmonic:agent/tool-loading-client@0.4.4;
    include cosmonic:agent/spawn-client@0.4.4;
}
```

There is no guest-side policy world: a component placed between a caller and
the implementer would become the caller, and its grants would become the
ceiling. Policy layers are installed by the host.

## Scenario 1: find a tool and use it in the same run

An agent working on a task finds it needs to read an archive.

```rust
let found = tool_discovery::list(Some("archive".into())).await?;
let offer = found.offers.into_iter().next().ok_or(NoTool)?;

match tool_loading::load(offer.spec, None).await {
    Ok(done) if done.toolset.state == ToolsetState::Offered => {
        // Callable now. List again so the model sees the new tools.
        let tools = tools::list().await?;
        // ...carry on with this run's next model call.
    }
    Ok(done) if done.next_run => { /* the host serves it from the next run: finish this one */ }
    Ok(_) => { /* still loading: check `loaded()` before the next step */ }
    Err(AdmissionError::NeedsApproval(approval)) => {
        // Returned at once. Finish the run asking the person, or keep working
        // and resend the same spec once `wait_for_decision` says granted.
    }
    Err(e) => return Err(e.into()),
}
```

Within the ceiling, for a toolset in the operator's catalog (or one already
in the session ceiling), `load` admits it at once, rolls the agent's
workload out to a revision that adds its component, and resolves when its
tools are callable. The run in progress keeps going on the same instance; the
old revision retires when the run ends. Beyond the ceiling, `load` returns
`needs-approval` at once, so a pending approval never holds the run. Once the
person allows it, the toolset joins the session ceiling: if it is later set
aside or unbound, the same toolset loads again without asking, as long as its
digest and resolved reach are unchanged.

A harness that loads tools, or lets its model call the host's `load_tools`
tool, calls `tools.list` again before its next model call. The host serves it
without running any guest code, and its model adapters send the change in the
cheapest form each provider allows, so the prompt cache survives where the
provider supports it.

## Scenario 2: stay within the model's function budget

```rust
let set = tool_loading::loaded().await;
// set.offered_functions = 29, set.max_offered_functions = Some(32)

let done = tool_loading::load(git_spec, None).await?;
// done.set_aside = ["archive"]: the least recently called toolset this agent
// loaded, set aside to make room. Its component and approval stay.

tool_loading::load(archive_spec, None).await?; // back to offered: no prompt, no rollout
tool_loading::unload("git".into()).await?;     // set aside by hand: ready, still approved
```

The budget comes from the model binding and the manifest may only lower it.
Pinned toolsets (the operator's, and those the person added or pinned) are
never set aside, nor is one loaded or called in the last few runs. When
nothing can make room, `load` answers `over-limit` with scope
`offered-functions`. `unload` sets aside; only the person removes a toolset
from the session, which also ends its approval.

## Scenario 3: delegate a review to a child session

A coding agent asks a reviewer for a structured verdict. The reviewer gets one
toolset, a file reader with read-only access to the workspace volume the coder
already reads, so the request fits the coder's ceiling.

```rust
#[derive(Deserialize, JsonSchema)]
struct Review { risks: Vec<String>, verdict: Verdict }

let request = SpawnRequest::component(pinned("ghcr.io/acme/agents/reviewer:1.2.0", "sha256:9f…"))
    .model_alias("frontier")                        // a model alias, never a key
    .instructions("Review the PR and list risks.")
    .toolset("repo-fs", pinned("ghcr.io/cosmonic/tools/fs:0.2.0", "sha256:1c…"),
          Grants::none().volume("workspace", "/src", ReadOnly))
    .output::<Review>().max_bytes(16_384)
    .budget(Budget { max_runs: Some(3), max_steps: Some(12), ..Default::default() })
    .label("role", "reviewer");

let child = spawn::spawn(request, Some(user("Start with the auth changes.")), "review-pr-42").await?;
let first = child.run.unwrap();                       // run id, progress, outcome
let review: Review = spawn::wait(&child.session, &first.run).await?.structured()?;
let next = spawn::send(&child.session, user("Now the billing changes.")).await?;
let more: Review = spawn::wait(&child.session, &next.run).await?.structured()?;
spawn::end(&child.session).await?;
```

The child is a session of its own, recorded under the coder's. The coder sees
contentless progress (`working`, `blocked`, step counts), then the final reply
and the structured verdict. It never sees the reviewer's tool calls, tool
output or conversation; a person sees those by opening the child under the
coder's session in the host's own view.

A child's reach rests on its ancestors. Its own agent component gets only
grants the parent already has; a toolset beyond the parent's ceiling asks the
person, and their approval is for this one child as shown (its output, budget
and powers), never something the parent can load or another child can reuse.
The key (`review-pr-42`) makes a resend return the same child instead of a
second one. A child's toolsets stay exact toolsets: it can hand one down,
never lend its reach to another component. The host re-checks a child against
its ancestors at every run, so a parent losing reach narrows its children
too. Each tree has limits on depth (3), runs at once (20) and live child
sessions (50), and a child's token budget covers everything below it.

## Scenario 4: work with children across runs and hosts

Children are addressed by session id through the host's session store, so
nothing depends on the store, instance or host that spawned them.

```rust
let researcher = spawn::spawn(
    request.placement(Placement::Zone("eu-west".into())),  // a hint, never reach
    Some(user("Collect the release notes for 2.12.")),
    "release-notes-2.12",
).await?;
let run = researcher.run.unwrap().run;
// ...this run ends; the caller keeps both ids in its own session...

// A later run, maybe on another instance or host:
let notes = spawn::wait(&researcher.session, &run).await?; // that run's outcome
for c in spawn::children(true).await? {                  // the whole subtree, parent first
    println!("{} under {} at depth {}: {:?} {:?}", c.session, c.parent, c.depth, c.status, c.failure);
}
```

Dropping a progress stream or outcome future only stops listening; only
`cancel` or `end` stops a child, and both reach everything below it (so does
a person's stop in the host's UI). If the host running a child goes away
mid-run, its lease in the session store expires: `wait` answers
`host-lost`, the child's state is `failed` with `failure` saying why, and a
later `send` starts a new run, placed again. Nothing hangs silently. The
lease protects the session and what the store mediates; a cut-off host may
still finish effects it had started with the child's own grants before it
fenced itself, so a new run can overlap those.

A parent acts only on its direct children (send, wait, cancel, end) and sees
its whole subtree's state; hosts draw the same tree in their session lists.
Children belong to the component that spawned them, so a sibling component
in the same session cannot drive them.

## Scenario 5: read untrusted content in quarantine

A privileged agent hands untrusted content (an inbound email, a scraped
page) to a child with no grants and accepts only a constrained answer.

```rust
#[derive(Deserialize, JsonSchema)]
struct Triage { category: Category, urgent: bool, confidence: u8 } // no strings

let reader = SpawnRequest::component(pinned(QUARANTINE_READER, "sha256:…"))
    .model_alias("local-small")
    .output::<Triage>().max_bytes(256).structured_only()
    .budget(Budget { max_runs: Some(1), max_steps: Some(1), ..Default::default() });
let child = spawn::spawn(reader, Some(user(&untrusted_email)), "triage-msg-881").await?;
let run = child.run.unwrap().run;
let triage: Triage = spawn::wait(&child.session, &run).await?.structured()?;
```

With `structured-only`, the host empties the reply text and fills every other
field of the outcome from its own records, so only the schema's fields come
back from the child. The host does not check that
the schema has no free strings. This narrows what a manipulated child can
say; it does not stop the child being manipulated, and nothing here blocks
prompt injection.

## Files

| File | Contents |
| --- | --- |
| [`spawn-types.wit`](../../spawn-types.wit) | The contract (caller, reach, ceiling, session ceiling, children, authorship) and the shared records |
| [`tool-discovery.wit`](../../tool-discovery.wit) | `tool-discovery` |
| [`tool-loading.wit`](../../tool-loading.wit) | `tool-loading` |
| [`spawn.wit`](../../spawn.wit) | `spawn`: child sessions by id |
| [`world.wit`](../../world.wit) | The worlds above, beside the package's others |
