//! Records the resolved versions of the tree-sitter crates (from the nearest `Cargo.lock`)
//! in `CARTOGRAPH_GRAMMAR_VERSIONS`, so the extraction-cache key changes when a grammar is
//! updated (`cargo update -p tree-sitter-typescript`), not only when this crate's version
//! does.

use std::path::{Path, PathBuf};

fn find_lock() -> Option<PathBuf> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let mut dir: Option<&Path> = Some(Path::new(&manifest));
    while let Some(d) = dir {
        let lock = d.join("Cargo.lock");
        if lock.is_file() {
            return Some(lock);
        }
        dir = d.parent();
    }
    None
}

/// `name@version` of every `tree-sitter*` package, sorted.
fn grammar_versions(lock: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    for line in lock.lines() {
        if line.starts_with("[[package]]") {
            name = None;
        } else if let Some(n) = line.strip_prefix("name = ") {
            name = Some(n.trim().trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("version = ")
            && let Some(n) = name.take()
            && n.starts_with("tree-sitter")
        {
            out.push(format!("{n}@{}", v.trim().trim_matches('"')));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let versions = match find_lock() {
        Some(lock) => {
            println!("cargo::rerun-if-changed={}", lock.display());
            match std::fs::read_to_string(&lock) {
                Ok(text) => grammar_versions(&text).join(","),
                Err(e) => {
                    println!("cargo::warning=cannot read {}: {e}", lock.display());
                    String::new()
                }
            }
        }
        None => String::new(),
    };
    // Without a lock file the key still carries the crate version; say so explicitly.
    let versions = if versions.is_empty() {
        "unlocked".to_string()
    } else {
        versions
    };
    println!("cargo::rustc-env=CARTOGRAPH_GRAMMAR_VERSIONS={versions}");
}
