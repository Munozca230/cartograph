//! Shared helpers: index a fixture repo from a temp copy, compare with goldens.

#![allow(dead_code)]

use cartograph_core::Graph;
use cartograph_index::{IndexOptions, index};
use cartograph_query::{Config, Repo};
use std::fs;
use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            fs::copy(entry.path(), &dst).unwrap();
        }
    }
}

/// Index `dir` (already on disk) in memory.
pub fn index_dir(dir: &Path) -> Graph {
    index(dir, &IndexOptions::default()).unwrap().graph
}

/// Index `tests/fixtures/<name>` from a temp copy, symbols included; the config is the
/// fixture's own `cartograph.toml` (or the defaults).
pub fn fixture(name: &str) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    copy_dir(&repo_root().join("tests/fixtures").join(name), &root);
    let out = index(&root, &IndexOptions::default()).unwrap();
    let config = Config::load(&root).unwrap();
    Repo::with_symbols(&out.graph, &out.symbols, config)
}

/// Compare `actual` with `tests/golden/<rel>`; `UPDATE_GOLDEN=1` rewrites it.
pub fn golden(rel: &str, actual: &str) {
    let path = repo_root().join("tests/golden").join(rel);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "missing golden {}; run with UPDATE_GOLDEN=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "golden {rel} differs; rerun with UPDATE_GOLDEN=1 and review the diff"
    );
}
