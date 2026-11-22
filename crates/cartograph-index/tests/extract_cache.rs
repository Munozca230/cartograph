//! Extraction cache: raw imports are reused for unchanged files, resolution always runs.

use cartograph_core::{Graph, Store};
use cartograph_index::{DB_FILE, IndexOptions, imports, index, index_to_store};
use std::path::{Path, PathBuf};

fn opts(db: &Path) -> IndexOptions {
    IndexOptions {
        data_dir: Some(db.to_path_buf()),
        ..IndexOptions::default()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), &dst).unwrap();
        }
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn a_new_file_resolves_an_old_unresolved_import_while_the_importer_is_a_cache_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let db = tmp.path().join("db");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.ts"), "import { b } from './b';\n").unwrap();

    let first = index_to_store(&repo, &opts(&db)).unwrap();
    assert_eq!(first.timings.cache_hits, 0);
    assert_eq!(first.timings.cache_misses, 1);
    assert_eq!(first.graph.unresolved.len(), 1, "./b does not exist yet");

    std::fs::write(repo.join("b.ts"), "export const b = 1;\n").unwrap();
    let second = index_to_store(&repo, &opts(&db)).unwrap();
    assert_eq!(second.timings.cache_hits, 1, "a.ts is unchanged");
    assert_eq!(second.timings.cache_misses, 1, "b.ts is new");
    assert!(
        second.graph.unresolved.is_empty(),
        "{:?}",
        second.graph.unresolved
    );
    assert_eq!(second.graph.imports.len(), 1);
    assert_eq!(second.graph.imports[0].target, "file:b.ts");

    // Editing a file makes it a miss again, and the old entry is pruned.
    std::fs::write(
        repo.join("a.ts"),
        "import { b } from './b';\nimport 'react';\n",
    )
    .unwrap();
    let third = index_to_store(&repo, &opts(&db)).unwrap();
    assert_eq!(
        (third.timings.cache_hits, third.timings.cache_misses),
        (1, 1)
    );
    assert_eq!(third.graph.imports.len(), 2);
    let store = Store::open(db.join(DB_FILE)).unwrap();
    assert_eq!(
        store
            .load_extract_cache(imports::extractor_version())
            .unwrap()
            .len(),
        2,
        "one entry per current (hash, lang)"
    );
}

#[test]
fn a_warm_run_produces_the_same_graph_as_an_uncached_one() {
    for name in ["imports-ts", "imports-py"] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixture(name), &repo);
        let db = tmp.path().join("db");
        let cold = index_to_store(&repo, &opts(&db)).unwrap();
        let warm = index_to_store(&repo, &opts(&db)).unwrap();
        assert_eq!(warm.timings.cache_misses, 0, "{name}");
        assert_eq!(warm.timings.cache_hits, cold.timings.cache_misses, "{name}");
        let plain: Graph = index(&repo, &opts(&db)).unwrap().graph;
        assert_eq!(warm.graph, plain, "{name}");
        assert_eq!(cold.graph, plain, "{name}");
    }
}

#[test]
fn a_damaged_cache_entry_is_reparsed_not_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let db = tmp.path().join("db");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.ts"), "import 'react';\n").unwrap();
    index_to_store(&repo, &opts(&db)).unwrap();
    rusqlite_exec(
        &db.join(DB_FILE),
        "UPDATE extract_cache SET imports = 'not json'",
    );
    let again = index_to_store(&repo, &opts(&db)).unwrap();
    assert_eq!(
        (again.timings.cache_hits, again.timings.cache_misses),
        (0, 1)
    );
    assert_eq!(again.graph.imports.len(), 1);
    let third = index_to_store(&repo, &opts(&db)).unwrap();
    assert_eq!(third.timings.cache_hits, 1, "the entry was rewritten");
}

fn rusqlite_exec(db: &Path, sql: &str) {
    let store = Store::open(db).unwrap();
    store.connection().execute_batch(sql).unwrap();
}

/// `(name, version)` of every `tree-sitter*` package in the workspace lock file.
fn locked_grammars() -> Vec<(String, String)> {
    let lock =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))
            .unwrap();
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    for line in lock.lines() {
        if let Some(n) = line.strip_prefix("name = ") {
            name = Some(n.trim_matches('"').to_string());
        } else if let (Some(v), Some(n)) = (line.strip_prefix("version = "), name.take())
            && n.starts_with("tree-sitter")
        {
            out.push((n, v.trim_matches('"').to_string()));
        }
    }
    out
}

#[test]
fn the_extractor_version_pins_every_resolved_grammar_version() {
    let locked = locked_grammars();
    assert!(locked.iter().any(|(n, _)| n == "tree-sitter-typescript"));
    let v = imports::extractor_version();
    for (name, version) in locked {
        assert!(
            v.contains(&format!("{name}@{version}")),
            "{name}@{version} missing from {v}"
        );
    }
}

#[test]
fn a_damaged_entry_of_a_deleted_file_is_pruned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let db = tmp.path().join("db");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.ts"), "import 'react';\n").unwrap();
    std::fs::write(repo.join("b.ts"), "import 'lodash';\n").unwrap();
    index_to_store(&repo, &opts(&db)).unwrap();
    rusqlite_exec(
        &db.join(DB_FILE),
        "UPDATE extract_cache SET imports = 'not json'",
    );
    std::fs::remove_file(repo.join("b.ts")).unwrap();
    index_to_store(&repo, &opts(&db)).unwrap();
    let store = Store::open(db.join(DB_FILE)).unwrap();
    let n: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM extract_cache", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "only a.ts's rewritten entry remains");
}

#[test]
fn a_cancelled_index_stops_with_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    copy_dir(&fixture("imports-ts"), &repo);
    let progress = std::sync::Arc::new(cartograph_index::Progress::default());
    progress.cancel();
    let o = IndexOptions {
        progress: Some(progress.clone()),
        ..opts(&tmp.path().join("db"))
    };
    let err = index_to_store(&repo, &o).expect_err("a cancelled index must fail");
    assert!(
        matches!(err, cartograph_index::IndexError::Cancelled),
        "{err}"
    );
    assert!(progress.get().0 == 0, "no file was read after cancellation");
}
