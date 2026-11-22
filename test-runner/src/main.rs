//! agent-test [--agent <component.wasm>] <test.yaml | directory>...
//!
//! Runs each test file (a directory: every .yaml in it, sorted) and prints
//! one line per test, then the failures. Exits 1 when any test fails.
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

fn files(arg: &Path) -> Result<Vec<PathBuf>> {
    if !arg.is_dir() {
        return Ok(vec![arg.to_path_buf()]);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(arg)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yaml" || e == "yml"))
        .collect();
    found.sort();
    Ok(found)
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut agent = None;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--agent" => match args.next() {
                Some(p) => agent = Some(PathBuf::from(p)),
                None => bail!("--agent needs a path"),
            },
            "-h" | "--help" => {
                println!("agent-test [--agent <component.wasm>] <test.yaml | directory>...");
                return Ok(());
            }
            _ => paths.extend(files(Path::new(&a))?),
        }
    }
    if paths.is_empty() {
        bail!("no test files given");
    }
    let mut failed = 0;
    for path in &paths {
        match agent_test::run_file(path, agent.as_deref()).await {
            Ok(r) if r.passed() => println!("ok   {} ({})", r.name, path.display()),
            Ok(r) => {
                failed += 1;
                println!("FAIL {} ({})", r.name, path.display());
                for f in &r.failures {
                    println!("       {f}");
                }
            }
            Err(e) => {
                failed += 1;
                println!("FAIL {}: {e:#}", path.display());
            }
        }
    }
    println!("{} passed, {failed} failed", paths.len() - failed);
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
