//! wasmtime's bindgen has no switch for WIT feature gates, and `run` is
//! `@unstable(feature = agent-run)` in cosmonic:agent@0.4.2. Copy wit/ (its
//! packages fetched by `wkg wit fetch` into wit/deps) with that gate removed
//! into wit-host/ (gitignored).
use std::fs;
use std::path::Path;

fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        let dest = to.join(name);
        if path.is_dir() {
            copy(&path, &dest)?;
        } else if path.extension().is_some_and(|e| e == "wit") {
            let ungated: String = fs::read_to_string(&path)?
                .lines()
                .filter(|l| !l.trim_start().starts_with("@unstable(feature = agent-run)"))
                .map(|l| format!("{l}\n"))
                .collect();
            fs::write(dest, ungated)?;
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let source = Path::new("wit");
    println!("cargo:rerun-if-changed=wit");
    if !source.join("deps").is_dir() {
        return Err(std::io::Error::other(
            "wit/deps is missing: run `WKG_CONFIG_FILE=../wit/wkg-registries.toml wkg wit fetch` here first",
        ));
    }
    let dest = Path::new("wit-host");
    let _ = fs::remove_dir_all(dest);
    copy(source, dest)
}
