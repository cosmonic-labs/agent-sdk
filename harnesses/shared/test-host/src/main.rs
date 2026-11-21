//! Scripted end-to-end checks for the agent harnesses that export
//! `cosmonic:agent`'s `run` and `compact`: each call gets a fresh instance and
//! only the host's session survives, as with any host. Every harness runs the
//! shared contract suite (agents.rs) on one session, then its own cases on
//! another. No model credentials are needed: the model is scripted.
//!
//! cargo run --locked -- codex ../../codex/dist/codex-harness.wasm
//! cargo run --locked -- pi ../../pi/dist/pi-harness.wasm
mod agents;
mod codex;
mod fixture;
mod host;

use anyhow::{Context as _, Result, bail, ensure};

use agents::{Covers, Tally, contract_suite};
use fixture::Fixture;

#[tokio::main]
async fn main() -> Result<()> {
    let usage = "usage: test-host <codex | pi> <component.wasm>";
    let harness = std::env::args().nth(1).context(usage)?;
    let path = std::env::args().nth(2).context(usage)?;
    // The shared contract's session, and the harness's own.
    let contract = Fixture::new(&path)?;
    let f = Fixture::new(&path)?;
    let mut t = Tally::default();
    match harness.as_str() {
        "codex" => {
            use codex::*;
            contract_suite(&mut t, &contract, Covers::All).await;
            t.case("a turn with a tool call", tool_turn(&f)).await;
            t.case("the next run continues the history", second_turn(&f)).await;
            t.case("an unknown tool gets Codex's unsupported-call output", unknown_tool(&f)).await;
            t.case("update_plan streams a plan event and stays in the agent", plan(&f)).await;
            t.case("a freeform tool gets the model's raw text unchanged", freeform(&f)).await;
            t.case("a refused model call fails the run, recorded", refused(&f)).await;
            t.case("pre-turn compaction at 90% of the window", compaction(&f)).await;
            t.case("a schema is sent as the response format and checked", structured_output(&f)).await;
            t.case("a requested summary leaves the person's messages, then the summary", requested_history(&f)).await;
            let unknown = Fixture::new(&path)?;
            t.case("an unknown tool outcome ends the run, and nothing is called again", agents::unknown_outcome_stops(&unknown)).await;
        }
        "pi" => {
            contract_suite(&mut t, &contract, Covers::BeforeLatestExchange).await;
            t.case("past 90% of the window a run compacts and streams compacted", agents::auto_threshold(&contract)).await;
            t.case("one exchange has nothing to summarize: done, silent", agents::requested_after_one_exchange(&f)).await;
        }
        other => bail!("no suite for {other}; {usage}"),
    }
    println!("{} passed, {} failed", t.passed, t.failed);
    ensure!(t.failed == 0, "{} case(s) failed", t.failed);
    for f in [&contract, &f] {
        ensure!(f.agent().open_models == 0, "model handles leaked: {}", f.agent().open_models);
    }
    Ok(())
}
