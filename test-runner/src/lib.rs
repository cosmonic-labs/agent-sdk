//! Deterministic tests for agents built on `cosmonic:agent`.
//!
//! A test file (YAML, `version: 1`) names an agent component, what it is
//! granted, a scripted model and the tools. The runner hosts the agent, drives
//! one run through its `run` export, records every tool call, refusal and
//! outbound request, and checks the test's `expect` against that record.
//! Nothing reaches a network and no model is called.
//!
//! Use the `agent-test` binary, or call [`run_file`] from a Rust test.
mod check;
mod gate;
mod host;
mod model;
mod record;
mod runner;
mod session;
pub mod spec;
mod tools;

use std::path::Path;

pub use check::check;
pub use runner::Finished;

/// The result of one test file.
pub struct Report {
    pub name: String,
    /// Empty when the test passed.
    pub failures: Vec<String>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Load and run one test file, with `agent` in place of the file's
/// component when given.
pub async fn run_file(path: &Path, agent: Option<&Path>) -> anyhow::Result<Report> {
    let test = spec::Test::load(path)?;
    let finished = runner::run(&test, agent).await?;
    Ok(Report {
        name: test.name.clone(),
        failures: check(&test.expect, &finished),
    })
}
