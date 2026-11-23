//! Bridges (`tests/fixtures/workspace-bridges`, and the single-repo `electron-app`): every
//! endpoint and matched bridge, compared with `tests/golden/bridges-*.json`; regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test bridges` and review the diff.
//!
//! The traps: a channel written as a constant of another file (resolved) next to one built
//! at run time (counted, never matched); a handler registered through a wrapper method of a
//! base class; a renderer call of a function the preload does not expose; a class name two
//! members declare, imported from a member that declares only one of them; an import of a
//! member's class without declaring its package; a dependency at another version than its
//! member builds.

use cartograph_core::{Confidence, EdgeKind, Graph, Symbols};
use cartograph_index::{IndexOptions, index};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> PathBuf {
    repo_root().join("tests/fixtures").join(name)
}

fn golden_view(s: &Symbols) -> Value {
    let endpoints: Vec<String> = s
        .endpoints
        .iter()
        .map(|e| {
            format!(
                "{} {} {:?} {}:{} [{}] {}{}{}",
                e.kind,
                e.role,
                e.key,
                e.file,
                e.line,
                e.confidence,
                e.label,
                e.symbol
                    .as_deref()
                    .map_or(String::new(), |x| format!(" sym {x}")),
                e.detail
                    .as_deref()
                    .map_or(String::new(), |d| format!(" ({d})"))
            )
        })
        .collect();
    let bridges: Vec<String> = s
        .bridges
        .iter()
        .map(|b| {
            format!(
                "{} {} {:?} {}:{} {} -> {}:{} {} [{}]",
                b.kind,
                b.subtype,
                b.key,
                b.src_file,
                b.src_line,
                b.src.as_deref().unwrap_or("-"),
                b.dst_file,
                b.dst_line,
                b.dst.as_deref().unwrap_or("-"),
                b.confidence
            )
        })
        .collect();
    json!({ "endpoints": endpoints, "bridges": bridges })
}

fn check_golden(name: &str, got: &Value) {
    let path = repo_root()
        .join("tests/golden")
        .join(format!("{name}.json"));
    let text = format!("{}\n", serde_json::to_string_pretty(got).unwrap());
    if std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display())),
    )
    .unwrap();
    if &want != got {
        let out = std::env::temp_dir().join(format!("cartograph-{name}.actual.json"));
        std::fs::write(&out, &text).unwrap();
        panic!(
            "{name}: differs from {} (actual in {}; UPDATE_GOLDEN=1 regenerates)",
            path.display(),
            out.display()
        );
    }
}

fn import(g: &Graph, src: &str, dst: &str) -> Option<Confidence> {
    g.edges
        .iter()
        .find(|e| e.kind == EdgeKind::Import && e.src == src && e.dst == dst)
        .map(|e| e.confidence)
}

#[test]
fn workspace_bridges_match_the_golden_list() {
    let out = index(&fixture("workspace-bridges"), &IndexOptions::default()).unwrap();
    check_golden("bridges-workspace", &golden_view(&out.symbols));
    let g = &out.graph;
    // Same class name in two members: the declared one wins, as a resolved import.
    assert_eq!(
        import(
            g,
            "file:api/src/main/java/com/acme/api/Billing.java",
            "file:lib/src/main/java/com/acme/lib/Money.java"
        ),
        Some(Confidence::Resolved)
    );
    assert_eq!(
        import(
            g,
            "file:api/src/main/java/com/acme/api/Billing.java",
            "file:legacy/src/main/java/com/acme/lib/Money.java"
        ),
        None
    );
    // A member it does not declare: still an import, as a guess.
    assert_eq!(
        import(
            g,
            "file:api/src/main/java/com/acme/api/Reports.java",
            "file:legacy/src/main/java/com/acme/legacy/OldPrices.java"
        ),
        Some(Confidence::Heuristic)
    );
    // A package another member builds, declared in package.json: its source.
    assert_eq!(
        import(
            g,
            "file:desktop/src/renderer/App.tsx",
            "file:ui/src/index.ts"
        ),
        Some(Confidence::Resolved)
    );
    // IPC bridges are derived edges; package bridges are not.
    let bridge_edges: Vec<(&str, &str)> = g
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Bridge)
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    assert!(bridge_edges.contains(&(
        "file:desktop/src/preload/index.ts",
        "file:desktop/src/main/docs.ts"
    )));
    assert!(!bridge_edges.iter().any(|(s, _)| s.ends_with("pom.xml")));
}

#[test]
fn a_single_electron_repo_has_ipc_bridges() {
    let out = index(&fixture("electron-app"), &IndexOptions::default()).unwrap();
    check_golden("bridges-electron-app", &golden_view(&out.symbols));
}

#[test]
fn bridges_turned_off_leave_no_rows_and_no_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    copy_dir(&fixture("electron-app"), &root);
    std::fs::write(root.join("cartograph.toml"), "[bridges]\nipc = false\n").unwrap();
    let out = index(&root, &IndexOptions::default()).unwrap();
    assert!(out.symbols.bridges.is_empty() && out.symbols.endpoints.is_empty());
    assert!(!out.graph.edges.iter().any(|e| e.kind == EdgeKind::Bridge));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            std::fs::copy(entry.path(), &dst).unwrap();
        }
    }
}
