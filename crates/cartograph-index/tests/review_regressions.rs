//! Regression tests for the code-review findings on the import graph.

use cartograph_core::{EdgeKind, Graph, Store};
use cartograph_index::resolve::npm;
use cartograph_index::{IndexOptions, index, index_to_store};
use serde_json::json;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        write(&tmp.path().join("repo"), rel, content);
    }
    tmp
}

fn graph(tmp: &tempfile::TempDir) -> Graph {
    index(&tmp.path().join("repo"), &IndexOptions::default())
        .unwrap()
        .graph
}

fn targets_of<'g>(g: &'g Graph, file: &str) -> Vec<(&'g str, &'g str)> {
    g.imports
        .iter()
        .filter(|s| s.file == file)
        .map(|s| (s.specifier.as_str(), s.target.as_str()))
        .collect()
}

fn unresolved_of<'g>(g: &'g Graph, file: &str) -> Vec<(&'g str, &'g str)> {
    g.unresolved
        .iter()
        .filter(|u| u.file == file)
        .map(|u| (u.specifier.as_str(), u.reason.as_str()))
        .collect()
}

fn assert_no_dangling(g: &Graph) {
    let ids: std::collections::HashSet<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
    for e in &g.edges {
        assert!(ids.contains(e.src.as_str()), "dangling src {e:?}");
        assert!(ids.contains(e.dst.as_str()), "dangling dst {e:?}");
    }
    for s in &g.imports {
        assert!(ids.contains(s.target.as_str()), "dangling site {s:?}");
    }
}

/// Finding 1: `from . import helper, CONST` at the repo root (no `__init__.py`) must not
/// emit `Target::Dir("")`; only the submodule resolves, the rest is unresolved.
#[test]
fn relative_import_at_the_repo_root_never_targets_the_root_dir() {
    let tmp = project(&[
        ("main.py", "from . import helper, CONST\n"),
        ("helper.py", "X = 1\n"),
    ]);
    let g = graph(&tmp);
    assert_eq!(
        targets_of(&g, "main.py"),
        vec![(".helper", "file:helper.py")]
    );
    assert_eq!(unresolved_of(&g, "main.py"), vec![(".CONST", "not_found")]);
    assert!(!g.edges.iter().any(|e| e.dst == "dir:" || e.src == "dir:"));
    assert_no_dangling(&g);
}

/// Finding 3: in a dir without `__init__.py`, `from . import missing_mod` is not_found,
/// not a silent Exact edge to the dir.
#[test]
fn relative_import_of_a_missing_name_from_a_namespace_dir_is_not_found() {
    let tmp = project(&[
        (
            "pkg/a.py",
            "from . import missing_mod\nfrom . import b, nope\n",
        ),
        ("pkg/b.py", "Y = 2\n"),
    ]);
    let g = graph(&tmp);
    assert_eq!(targets_of(&g, "pkg/a.py"), vec![(".b", "file:pkg/b.py")]);
    let mut un = unresolved_of(&g, "pkg/a.py");
    un.sort();
    assert_eq!(
        un,
        vec![(".missing_mod", "not_found"), (".nope", "not_found")]
    );
    assert!(
        !g.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Import && e.dst == "dir:pkg")
    );
    assert_no_dangling(&g);
}

/// Finding 2: `.`, `..` and trailing-slash specifiers name a directory: resolve to its
/// `index.*`, never to a sibling `<dir>.ts`.
#[test]
fn directory_specifiers_go_straight_to_index() {
    let tmp = project(&[
        ("src/utils.ts", "export const wrong = 1;\n"),
        ("src/utils/index.ts", "export const right = 1;\n"),
        ("src/utils/a.ts", "import { right } from '.';\n"),
        ("src/utils/deep/b.ts", "import { right } from '..';\n"),
        ("src/main.ts", "import { right } from './utils/';\n"),
    ]);
    let g = graph(&tmp);
    for f in ["src/utils/a.ts", "src/utils/deep/b.ts", "src/main.ts"] {
        let t = targets_of(&g, f);
        assert_eq!(t.len(), 1, "{f}: {t:?}");
        assert_eq!(t[0].1, "file:src/utils/index.ts", "{f}");
    }
}

/// Finding 4: among matching `exports` wildcard patterns, the longest prefix wins.
#[test]
fn exports_wildcards_pick_the_longest_prefix() {
    let e = json!({
        "./*": "./src/*.js",
        "./features/*": "./src/features/*.ts",
    });
    assert_eq!(
        npm::export_targets(&e, "./features/x"),
        vec!["./src/features/x.ts"]
    );
    assert_eq!(npm::export_targets(&e, "./other"), vec!["./src/other.js"]);
}

/// Finding 5: resolver config problems are diagnostics, not skipped files.
#[test]
fn tsconfig_problems_are_diagnostics_not_skipped_files() {
    let tmp = project(&[
        (
            "tsconfig.json",
            "{ \"extends\": \"@missing/tsconfig\", \"compilerOptions\": {} }\n",
        ),
        ("a.ts", "export const a = 1;\n"),
    ]);
    let root = tmp.path().join("repo");
    let opts = IndexOptions {
        data_dir: Some(tmp.path().join("db")),
        ..IndexOptions::default()
    };
    let g = index_to_store(&root, &opts).unwrap().graph;
    assert!(
        !g.skipped.iter().any(|s| s.path == "tsconfig.json"),
        "{:?}",
        g.skipped
    );
    assert!(
        g.diagnostics
            .iter()
            .any(|d| d.path == "tsconfig.json" && d.detail.contains("@missing/tsconfig")),
        "{:?}",
        g.diagnostics
    );
    let store = Store::open(tmp.path().join("db/graph.db")).unwrap();
    assert_eq!(store.load_graph().unwrap().diagnostics, g.diagnostics);
    let md = cartograph_index::report::render(&store).unwrap();
    assert!(md.contains("## Skipped (0)"), "{md}");
    assert!(md.contains("## Diagnostics (1)"), "{md}");
}

/// Finding 6: `--data-dir` equal to the repo root keeps the database files out of the walk.
#[test]
fn data_dir_equal_to_the_repo_root_excludes_the_database_files() {
    let tmp = project(&[("a.py", "import os\n")]);
    let root = tmp.path().join("repo");
    let opts = IndexOptions {
        data_dir: Some(root.clone()),
        ..IndexOptions::default()
    };
    index_to_store(&root, &opts).unwrap();
    // The MCP server's usage log and its rotation live there too, and the base snapshots.
    std::fs::write(root.join("stats.jsonl"), "{}\n").unwrap();
    std::fs::write(root.join("stats.1.jsonl"), "{}\n").unwrap();
    let snapshots = root.join(cartograph_index::git::snapshot::SNAPSHOT_DIR);
    std::fs::create_dir_all(&snapshots).unwrap();
    std::fs::write(snapshots.join("0123abcd.json"), "{}\n").unwrap();
    // Second run: graph.db (and maybe -wal / -shm) now exist in the root.
    let g = index_to_store(&root, &opts).unwrap().graph;
    assert!(root.join("graph.db").is_file());
    for n in &g.nodes {
        assert!(
            !n.path.starts_with("graph.db")
                && !n.path.starts_with("stats")
                && !n
                    .path
                    .starts_with(cartograph_index::git::snapshot::SNAPSHOT_DIR),
            "data file indexed: {}",
            n.path
        );
    }
    // Excluded up front, never read (and sniffed as binary) while it is being written.
    let db_entries: Vec<_> = g
        .skipped
        .iter()
        .filter(|s| s.path.starts_with("graph.db"))
        .collect();
    assert!(!db_entries.is_empty());
    for s in db_entries {
        assert_eq!(s.reason, cartograph_core::SkipReason::ExcludedDir, "{s:?}");
    }
}

/// Finding 7: the fan-in table counts files only (a namespace-package Dir is not a file).
#[test]
fn fan_in_table_lists_files_only() {
    let tmp = project(&[
        ("pkg/__init__.py", ""),
        ("pkg/ns/mod.py", "X = 1\n"),
        ("pkg/a.py", "import pkg.ns\n"),
        ("pkg/b.py", "import pkg.ns\n"),
    ]);
    let root = tmp.path().join("repo");
    let opts = IndexOptions {
        data_dir: Some(tmp.path().join("db")),
        ..IndexOptions::default()
    };
    let g = index_to_store(&root, &opts).unwrap().graph;
    assert!(
        g.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Import && e.dst == "dir:pkg/ns"),
        "fixture must produce a namespace Dir target"
    );
    let store = Store::open(tmp.path().join("db/graph.db")).unwrap();
    let md = cartograph_index::report::render(&store).unwrap();
    let fan_in = md
        .split("### Top files by fan-in")
        .nth(1)
        .and_then(|s| s.split("###").next())
        .unwrap();
    assert!(!fan_in.contains("pkg/ns |"), "{fan_in}");
}

/// Finding 8: flow-style lists in pnpm-workspace.yaml.
#[test]
fn pnpm_flow_style_lists() {
    assert_eq!(
        npm::pnpm_workspace_globs("packages: ['apps/*', \"libs/*\"]\n"),
        vec!["apps/*", "libs/*"]
    );
    assert_eq!(
        npm::pnpm_workspace_globs("packages: [\n  'apps/*',\n  '!apps/old',\n]\nother: 1\n"),
        vec!["apps/*", "!apps/old"]
    );
    // Block style still works.
    assert_eq!(
        npm::pnpm_workspace_globs("packages:\n  - 'pkgs/*'\n"),
        vec!["pkgs/*"]
    );
}
