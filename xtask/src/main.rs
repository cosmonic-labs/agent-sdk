use std::env;
use std::io;
use std::path::Path;
use std::process::Command;

const AGENT: &str = "components/minimal-agent";
const FETCH_TOOL: &str = "tests/fixtures/tools/fetch-tool";
const RUNNER: &str = "tests/runner";

fn run(command: &mut Command) -> io::Result<()> {
    eprintln!("Running {command:?}");
    let status = command.status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "{command:?} exited with {status}"
        )));
    }
    Ok(())
}

fn main() -> io::Result<()> {
    if env::args().skip(1).collect::<Vec<_>>() != ["agent-tests"] {
        return Err(io::Error::other("usage: cargo xtask agent-tests"));
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    env::set_current_dir(root)?;
    run(Command::new("node").args([
        "harnesses/shared/scripts/fetch-wit.mjs",
        AGENT,
        FETCH_TOOL,
        RUNNER,
    ]))?;

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for dir in [AGENT, FETCH_TOOL] {
        run(Command::new(&cargo)
            .current_dir(dir)
            .args([
                "build",
                "--locked",
                "--release",
                "--target",
                "wasm32-wasip2",
                "--target-dir",
            ])
            .arg(root.join(RUNNER).join("target")))?;
    }
    run(Command::new(cargo)
        .current_dir(RUNNER)
        .args(["test", "--locked"]))
}
