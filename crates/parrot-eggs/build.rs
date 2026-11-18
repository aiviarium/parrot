//! Compiles the in-repo registry `eggs/` into the crate, as the fallback when nothing is installed.

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../eggs");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("package.kdl").is_file() {
                walk(&root, &path, &mut files);
            }
        }
    }
    files.sort();
    let mut out = String::from(
        "/// Every file of the in-repo eggs, by path relative to `eggs/`.\npub static FILES: &[(&str, &[u8])] = &[\n",
    );
    for (rel, abs) in files {
        out.push_str(&format!("    ({rel:?}, include_bytes!({abs:?})),\n"));
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("builtin.rs");
    std::fs::write(dest, out).unwrap();
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, files);
        } else if let Ok(rel) = path.strip_prefix(root) {
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let abs = path.canonicalize().unwrap_or(path.clone());
            files.push((rel, abs.to_string_lossy().into_owned()));
        }
    }
}
