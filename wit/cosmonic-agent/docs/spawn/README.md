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
- **spawn**: start a child agent and read its final replies.

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
| caller | The component an import is bound to: its workload incarnation and its component name. |

## Worlds

| World | Imports | Exports | Who targets it |
| --- | --- | --- | --- |
| `tool-discovery-client` | `tool-discovery` | none | Included by an agent that may browse the catalog |
| `tool-loading-client` | `tool-loading` | none | Included by an agent that may change its own toolsets |
| `spawn-client` | `spawn` | none | Included by an agent that may start child agents |
| `spawning-agent` | `runnable-agent` and all three | `run` | A runnable agent with every power |
| `tool-discovery-provider` | none | `tool-discovery` | An implementer, or a test double |
| `tool-loading-provider` | none | `tool-loading`, `tools` | An implementer, which must also serve the caller's `tools` |
| `spawn-provider` | none | `spawn` | An implementer, or a test double |

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
    Ok(_) => { /* still loading: check `loaded()` before the next step */ }
    Err(AdmissionError::NeedsApproval(approval)) => {
        // Returned at once. Finish the run asking the person, or keep working
        // and resend the same spec once `wait_for_decision` says granted.
    }
    Err(e) => return Err(e.into()),
}
```

Within the ceiling, `load` admits the toolset at once, rolls the agent's
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

## Scenario 3: delegate a review and read the verdict

A coding agent asks a reviewer for a structured verdict. The reviewer gets one
toolset, a file reader with read-only access to the workspace volume the coder
already reads, so the request fits the coder's ceiling.

```rust
#[derive(Deserialize, JsonSchema)]
struct Review { risks: Vec<String>, verdict: Verdict }

let reviewer = Agent::new(pinned("ghcr.io/acme/agents/reviewer:1.2.0", "sha256:9f…"))
    .model_alias("frontier")                        // a model alias, never a key
    .instructions("Review the PR and list risks.")
    .toolset("repo-fs", pinned("ghcr.io/cosmonic/tools/fs:0.2.0", "sha256:1c…"),
          Grants::none().volume("workspace", "/src", ReadOnly))
    .output::<Review>().max_bytes(16_384)
    .budget(Budget { max_runs: Some(3), max_steps: Some(12), ..Default::default() })
    .label("role", "reviewer")
    .spawn().await?;

let review: Review = reviewer.run(user("Start with the auth changes.")).await?.structured()?;
drop(reviewer); // ends the child
```

The coder sees contentless progress (`working`, `blocked`, step counts), then
the final reply and the structured verdict. It never sees the reviewer's tool
calls, tool output or conversation; a person sees those in the host's view of
the child's session, listed under the coder's.

A child's reach never exceeds its parent's ceiling. Its own agent component
gets only grants the parent already has; a toolset beyond the parent's
ceiling asks the person, and their approval adds it to the parent's session
ceiling. The spawn tree has a depth limit (default 3) and a limit on children
running at once (default 20).

## Scenario 4: keep a child across runs

A harness gets a fresh store for each run, so a handle cannot outlive the run.
`detach` keeps the child in the session, and a later run attaches to it.

```rust
let researcher = spawn::spawn(request).await?;
let id = researcher.get_id();
spawn::Agent::detach(researcher);           // keeps running, keeps its budget
// ...the run ends; the id is kept in the session...

let researcher = spawn::attach(&id).await?; // a later run picks it up
for child in spawn::children().await {      // each with its host-derived status
    println!("{} {:?} attached={}", child.id, child.status, child.attached);
}
```

Only the same caller in the same session can attach; the id authorizes
nothing on its own. A detached child ends after an idle period, when its
budget runs out, or with the session.

## Scenario 5: read untrusted content in quarantine

A privileged agent hands untrusted content (an inbound email, a scraped
page) to a child with no grants and accepts only a constrained answer.

```rust
#[derive(Deserialize, JsonSchema)]
struct Triage { category: Category, urgent: bool, confidence: u8 } // no strings

let reader = Agent::new(pinned(QUARANTINE_READER, "sha256:…"))
    .model_alias("local-small")
    .output::<Triage>().max_bytes(256).structured_only()
    .budget(Budget { max_runs: Some(1), max_steps: Some(1), ..Default::default() })
    .spawn().await?;
let triage: Triage = reader.run(user(&untrusted_email)).await?.structured()?;
```

With `structured-only`, the host empties the reply text before the parent
sees it, so only the schema's fields come back. The host does not check that
the schema has no free strings. This narrows what a manipulated child can
say; it does not stop the child being manipulated, and nothing here blocks
prompt injection.

## Files

| File | Contents |
| --- | --- |
| [`spawn-types.wit`](../../spawn-types.wit) | The contract (caller, reach, ceiling, session ceiling, children, authorship) and the shared records |
| [`tool-discovery.wit`](../../tool-discovery.wit) | `tool-discovery` |
| [`tool-loading.wit`](../../tool-loading.wit) | `tool-loading` |
| [`spawn.wit`](../../spawn.wit) | `spawn` and the `agent` resource |
| [`world.wit`](../../world.wit) | The worlds above, beside the package's others |
