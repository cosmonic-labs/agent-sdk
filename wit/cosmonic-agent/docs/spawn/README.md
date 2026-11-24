# Spawn and tool loading

**Status: unstable.** Part of `cosmonic:agent` 0.4.4. Tool discovery and
loading are behind the `agent-tool-loading` feature. Lineage, children and
spawn are behind `agent-spawn`, which also needs `agent-tool-loading`,
`agent-run` and `session-status`. For wit-bindgen, list the features you need
(`features: ["agent-tool-loading", "agent-spawn", "agent-run",
"session-status"]`); a Wasmtime host sets the same options on the
`LinkOptions` its generated `add_to_linker` takes. Why things are the way
they are is in [RATIONALE.md](RATIONALE.md); what an implementer must do is
in [CONFORMANCE.md](CONFORMANCE.md).

An agent with a job often needs help it did not start with: a tool it finds
it needs halfway through a task, or a second agent to review one file or to
read content it should not trust. These interfaces give an agent separate
powers, each its own import, so its world says exactly what it may do:

| Interface | The agent may |
| --- | --- |
| `tool-discovery` | ask which tool components are available |
| `tool-loading` | load tool components into itself and set them aside |
| `lineage` | read its place in a spawn tree and its remaining budget (every spawned child may) |
| `child-sessions` | see the sessions below it |
| `spawn` | start child agent sessions and drive them |

A **tool component** is one component with its own grants that offers tools. Its
**reach** is its grants limited to the interfaces it imports, read from the
binary before it runs. A **child** is an agent session of its own, in its own
workload, inheriting nothing.

## Imports

These interfaces come with no worlds of their own:
a component imports the ones it needs into its own world,
so the import list shows exactly which powers it holds.

| Component | Usually imports | Exports |
| --- | --- | --- |
| An agent that finds and loads its own tools | `tool-discovery`, `tool-loading` | `run` |
| An agent that delegates to children | `spawn`, `child-sessions`, `lineage` | `run` |
| A spawned child | `lineage`, and the powers its parent gave it | `run` |
| A component that watches a subtree without driving it | `child-sessions` | |
| An implementer or a test double | | the interfaces it serves, and `tools` with `tool-loading` |

An agent builds on `runnable-agent` and adds the powers it needs:

```wit
package acme:coder@1.0.0;

world coder {
    include cosmonic:agent/runnable-agent@0.4.4;

    import cosmonic:agent/tool-loading@0.4.4;
    import cosmonic:agent/lineage@0.4.4;
    import cosmonic:agent/spawn@0.4.4;
}
```

## Scenario 1: find a tool and use it in the same run

```rust
let found = tool_discovery::list_offers(Some("archive".into())).await?;
let offer = found.offers.first().ok_or(NoTool)?;
// offer.imports lists what the component can call: here, the filesystem only.

let done = tool_loading::load(ToolComponentSource::Catalog(CatalogEntry { name: offer.name.clone() })).await?;
match done.tool_component.state {
    ToolComponentState::Offered => {
        let tools = tools::list().await?; // list again: the new tools are there
        // ...carry on with this run's next model call
    }
    ToolComponentState::NextRun => { /* finish this run; the tools arrive on the next */ }
    ToolComponentState::AwaitingApproval(p) => { /* finish the run asking the person: p.summary */ }
    _ => {} // `ready` and others are returned only for a tool component already held
}
// A rollout that fails or runs out of time is an error (`unavailable`).
```

`load` resolves once the tool component is callable, will be on the next run, or
waits on a grantor, so there is nothing to poll. Within the caller's ceiling,
a catalog tool component loads without asking and serves the run in progress. Beyond it, `load` returns `awaiting-approval` without
waiting. A person's approval lasts for the session: setting the tool component aside
and loading it again later needs no prompt, until the person removes it.

## Scenario 2: stay within the model's function budget

```rust
fn catalog(name: &str) -> CatalogEntry { CatalogEntry { name: name.into() } }

let set = tool_loading::list_loaded().await?;
// set.offered_functions = 29, set.max_offered_functions = Some(32)

let done = tool_loading::load(ToolComponentSource::Catalog(catalog("git"))).await?;
// done.set_aside = ["archive"]: the least recently called tool component this agent
// loaded, set aside to make room. Its component and approval stay.

tool_loading::load(ToolComponentSource::Catalog(catalog("archive"))).await?; // back at once
tool_loading::set_aside("git".into()).await?;                       // ready, still approved
```

## Scenario 3: delegate a review to children

```rust
#[derive(Deserialize, JsonSchema)]
struct Review { risks: Vec<String>, verdict: Verdict }

let request = SpawnRequest {
    agent: AgentSource::Custom(AgentSpec {
        component: pinned("ghcr.io/acme/agents/reviewer:1.2.0", "sha256:9f…"),
        model_alias: "frontier".into(),
        grants: Grants::none(),
        tool_components: vec![ToolComponentSource::Catalog(catalog("repo-fs-readonly"))],
    }),
    instructions: "Review the PR and list risks.".into(),
    output: OutputShape::ReplyAndStructured(schema_for::<Review>()),
    max_output_bytes: 16_384, // caps each run's encoded outcome
    budget: SpawnBudget { max_runs: Some(3), max_steps: Some(12), ..Default::default() },
    powers: AgentPowers::empty(),
};
let first = |msg: &str| Some(FirstRun { run: "review".into(), message: user(msg) });

// Two reviewers start and work at once; neither waits on the other.
let (auth, billing) = futures::join!(
    spawn::spawn("reviewer-pr-42-auth", request.clone(), first("Review the auth changes.")),
    spawn::spawn("reviewer-pr-42-billing", request, first("Review the billing changes.")),
);
let (auth, billing) = (auth?, billing?);
let (a, b) = futures::join!(auth.wait("review"), billing.wait("review"));
let (auth_review, billing_review): (Review, Review) = (a?.structured()?, b?.structured()?);
futures::try_join!(ChildSession::end(auth), ChildSession::end(billing))?;
```

A child takes one run at a time, so work that should run in parallel goes to
separate children.
Names are `[a-z0-9-]`.
The name is the child's address and its idempotency key:
spawning `reviewer-pr-42-auth` again returns the same child,
and sending run `review` to it again returns the same run.
If the request needs a grantor, the child exists in state `pending`
and its first run waits as `blocked(admission)` until the grant.
`max-output-bytes` bounds what comes back from each run:
an outcome larger than that fails `output-rejected`.
The parent sees contentless progress and the final outcome,
never a reviewer's tool calls or conversation.

## Scenario 4: collect children's results in a later run

```rust
// Run 1: start three researchers and end this run.
for topic in ["pricing", "competitors", "regulation"] {
    let c = spawn::spawn(&format!("research-{topic}"), request_for(topic),
        Some(FirstRun { run: "r1".into(), message: user(topic) })).await?;
    drop(c); // releases the reference; the child keeps working
}

// Run 2, maybe on another machine: where does each stand?
for info in child_sessions::list_children(ChildQuery { descendants: false }).await? {
    if let Some((run, RunState::Done)) = &info.last_run {
        let child = spawn::get_child(info.name.as_deref().unwrap()).await?;
        let notes = child.wait(run).await?;
        // ...
    }
}
```

`watch-children` streams the same records within a run. A child may run on
another machine; if that machine is lost mid-run, its `last-run` becomes
`failed(host-lost)` rather than hanging, and a later `send` starts a new run.
`cancel` and `end` reach everything below the child.

## Scenario 5: read untrusted content in quarantine

```rust
let reader = SpawnRequest {
    agent: AgentSource::Catalog(catalog("quarantine-reader")), // imports only chat
    output: OutputShape::StructuredOnly(schema_for::<Triage>()), // no free strings
    max_output_bytes: 256,
    budget: SpawnBudget { max_runs: Some(1), max_steps: Some(1), ..Default::default() },
    ..base
};
let child = spawn::spawn("triage-msg-881", reader,
    Some(FirstRun { run: "triage".into(), message: user(&untrusted_email) })).await?;
let triage: Triage = child.wait("triage").await?.structured()?;
```

With `structured-only`, the parent receives only the validated structured
value. The reader's component imports nothing beyond `chat`, which the
approver can see, and its prompts reach only its model's provider (use a
model binding without provider-run tools). An import list shows what a
component can call directly, not where data flows. This narrows what a
manipulated child can do and say; it does not stop it being manipulated.

## Files

| File | Contents |
| --- | --- |
| [`tool-component-types.wit`](../../tool-component-types.wit) | Grants, tool component sources and states |
| [`tool-discovery.wit`](../../tool-discovery.wit) | `tool-discovery` |
| [`tool-loading.wit`](../../tool-loading.wit) | `tool-loading` |
| [`spawn-types.wit`](../../spawn-types.wit) | Child state, runs, budgets |
| [`lineage.wit`](../../lineage.wit) | `lineage` |
| [`child-sessions.wit`](../../child-sessions.wit) | `child-sessions` |
| [`spawn.wit`](../../spawn.wit) | `spawn` and the `child-session` resource |
