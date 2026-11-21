//! wasmtime's bindgen has no switch for WIT feature gates, and `run` is
//! `@unstable(feature = agent-run)` in cosmonic:agent@0.4.2. Copy this
//! crate's world (wit/, its packages fetched by scripts/fetch-wit.mjs) with
//! the gates removed into wit-host/ (gitignored).
use std::fs;
use std::path::Path;

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if path.is_dir() {
            copy(&path, &dest);
        } else if path.extension().is_some_and(|e| e == "wit") {
            let text = fs::read_to_string(&path).unwrap();
            let ungated: String = text
                .lines()
                .filter(|l| !l.trim_start().starts_with("@unstable(feature = agent-run)"))
                .map(|l| format!("{l}\n"))
                .collect();
            fs::write(dest, ungated).unwrap();
        }
    }
}

fn main() {
    let source = Path::new("wit");
    println!("cargo:rerun-if-changed=wit");
    let dest = Path::new("wit-host");
    let _ = fs::remove_dir_all(dest);
    copy(source, dest);
}
