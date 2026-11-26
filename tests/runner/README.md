# Agent tests

The `agent-test` crate hosts an agent component with a scripted model, an in-memory session and the tools each test grants. Rust integration tests drive the agent through its `run` export and inspect the host's record of dispatched calls, refusals, HTTP requests and journal entries. Granted HTTP requests receive fixture responses. The tests need no model credentials or live services.

## Run the suite

Install Node.js, Rust with the `wasm32-wasip2` target, and `wkg` 0.16.0. From the repository root, run:

```shell
cargo xtask agent-tests
```

The task fetches locked WIT dependencies, builds minimal-agent and the [fetch-tool fixture](../fixtures/tools/fetch-tool), and runs the crate's unit and integration tests. After the first build, run individual cases with Cargo:

```shell
cargo test --manifest-path tests/runner/Cargo.toml ungranted_tool_is_refused_before_dispatch
```

## Write a case

Add a `#[tokio::test]` under [tests/](tests). Construct a `Scenario` with the agent component, user message and scripted replies. Set the agent's tool grants and each tool component's network or folder grants, then call `agent_test::run(&scenario).await?`. The returned `Finished` contains the agent's typed outcome, the host's record and the ordered journal entry kinds. A fixture fault, such as exhausting the model script, returns an error.

The [minimal-agent cases](tests/minimal_agent.rs) show how to assert on the actual arguments and result of a call, a refused tool or HTTP request, the contents of a granted folder, and an unknown tool outcome.
