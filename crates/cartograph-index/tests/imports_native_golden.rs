//! Golden tests for N1 import resolution of Rust, C and C++ (same scheme as
//! `imports_golden.rs`).
//!
//! Each fixture under `tests/fixtures/{rust,c,cpp}-*` is indexed from a temp copy and its
//! import edges, derived edges, import sites, unresolved imports, externals and config
//! diagnostics are compared exactly (sorted) with `tests/golden/<fixture>.json`.
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test
//! imports_native_golden` and review the diff.
//!
//! Hand-written assertions pin the cases each golden file must keep.

use cartograph_core::{Confidence, EdgeKind, Graph, NodeKind};
use cartograph_index::{IndexOptions, index};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
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

/// A temp copy of `tests/fixtures/<name>` (kept alive by the returned guard).
fn copy_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    copy_dir(&repo_root().join("tests/fixtures").join(name), &root);
    (tmp, root)
}

fn index_fixture(name: &str) -> Graph {
    let (_tmp, root) = copy_fixture(name);
    index(&root, &IndexOptions::default()).unwrap().graph
}

/// The import-related part of a graph, as sorted JSON rows.
fn golden_view(g: &Graph) -> Value {
    let edges: Vec<Value> = g
        .edges
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EdgeKind::Import | EdgeKind::ImportExternal | EdgeKind::DependsOn
            ) || (e.kind == EdgeKind::Contains && e.src.starts_with("package:"))
        })
        .map(|e| json!([e.src, e.dst, e.kind, e.confidence, e.weight]))
        .collect();
    let sites: Vec<Value> = g
        .imports
        .iter()
        .map(|s| json!([s.file, s.line, s.specifier, s.target]))
        .collect();
    let unresolved: Vec<Value> = g
        .unresolved
        .iter()
        .map(|u| json!([u.file, u.line, u.specifier, u.reason, u.detail]))
        .collect();
    let externals: Vec<&str> = g
        .nodes_of(NodeKind::External)
        .map(|n| n.id.as_str())
        .collect();
    let diagnostics: Vec<Value> = g
        .diagnostics
        .iter()
        .map(|d| json!([d.path, d.detail]))
        .collect();
    json!({
        "edges": edges,
        "sites": sites,
        "unresolved": unresolved,
        "externals": externals,
        "diagnostics": diagnostics,
    })
}

fn check_golden(name: &str, g: &Graph) {
    let path = repo_root()
        .join("tests/golden")
        .join(format!("{name}.json"));
    let got = golden_view(g);
    let text = format!("{}\n", serde_json::to_string_pretty(&got).unwrap());
    if std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &text).unwrap();
        return;
    }
    let want: Value = serde_json::from_str(
        &fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display())),
    )
    .unwrap();
    if want != got {
        let diff_path = std::env::temp_dir().join(format!("cartograph-{name}.actual.json"));
        fs::write(&diff_path, &text).unwrap();
        panic!(
            "{name}: graph differs from {} (actual written to {}; UPDATE_GOLDEN=1 regenerates)",
            path.display(),
            diff_path.display()
        );
    }
}

fn edge(g: &Graph, src: &str, dst: &str) -> Option<(EdgeKind, Confidence, u64)> {
    g.edges
        .iter()
        .find(|e| e.src == src && e.dst == dst && e.kind != EdgeKind::Contains)
        .map(|e| (e.kind, e.confidence, e.weight))
}

/// `(line, target)` of the import site of `file` with `spec`.
fn site<'g>(g: &'g Graph, file: &str, spec: &str) -> Option<(u32, &'g str)> {
    g.imports
        .iter()
        .find(|s| s.file == file && s.specifier == spec)
        .map(|s| (s.line, s.target.as_str()))
}

/// `(line, reason, detail)` of the unresolved import of `file` with `spec`.
fn unresolved<'g>(g: &'g Graph, file: &str, spec: &str) -> Option<(u32, &'g str, &'g str)> {
    g.unresolved
        .iter()
        .find(|u| u.file == file && u.specifier == spec)
        .map(|u| (u.line, u.reason.as_str(), u.detail.as_deref().unwrap_or("")))
}

use Confidence::{Exact, Heuristic, Resolved};
use EdgeKind::{Import, ImportExternal};

#[test]
fn crlf_fixture_file_kept_its_line_endings() {
    let f = "tests/fixtures/rust-workspace/crates/local-util/src/crlf/mod.rs";
    let bytes = fs::read(repo_root().join(f)).unwrap();
    assert!(
        bytes.windows(2).any(|w| w == b"\r\n"),
        "{f} lost its CRLF line endings"
    );
}

#[test]
fn rust_workspace_golden() {
    let g = index_fixture("rust-workspace");
    check_golden("rust-workspace", &g);

    let core = |p: &str| format!("file:crates/core-model/src/{p}");
    let lib = core("lib.rs");
    // `mod` declarations: file modules, `mod.rs`, #[path], modules inside inline modules.
    assert_eq!(
        edge(&g, &lib, &core("graph.rs")),
        Some((Import, Exact, 2)),
        "mod graph; + pub use graph::*"
    );
    assert_eq!(
        edge(&g, &lib, &core("store/mod.rs")),
        Some((Import, Exact, 3)),
        "mod store; + pub use store::Store + use crate::store::Store as S"
    );
    assert_eq!(
        edge(&g, &lib, &core("gen/ids.rs")),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(&g, &core("graph.rs"), &core("graph/node.rs")),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(
            &g,
            &core("store/mod.rs"),
            &core("store/inline_mod/deeper.rs")
        ),
        Some((Import, Exact, 2)),
        "mod inline_mod {{ pub mod deeper; }} + use self::inline_mod::deeper::Deep"
    );
    assert_eq!(
        unresolved(&g, "crates/core-model/src/lib.rs", "mod missing").map(|u| u.1),
        Some("not_found")
    );
    // `use super::*` in `mod tests`: the file itself, a site without a self-loop edge.
    assert_eq!(
        site(&g, "crates/core-model/src/lib.rs", "super::*"),
        Some((16, lib.as_str()))
    );
    assert!(edge(&g, &lib, &lib).is_none());
    // Paths: super/self/crate, nested groups, longest module prefix.
    assert_eq!(
        edge(&g, &core("graph/node.rs"), &core("graph/edge.rs")),
        Some((Import, Resolved, 2)),
        "use super::edge::Edge + use crate::{{graph::edge, …}}"
    );
    assert_eq!(
        site(&g, "crates/app/tests/common/mod.rs", "super::*"),
        Some((1, "file:crates/app/tests/smoke.rs")),
        "`super` of a module of an integration test is the test crate root"
    );
    assert_eq!(
        site(
            &g,
            "crates/core-model/src/graph/node.rs",
            "crate::ids::NodeId"
        ),
        Some((2, core("gen/ids.rs").as_str()))
    );
    assert_eq!(
        edge(&g, &core("store/sqlite.rs"), &core("graph/node.rs")),
        Some((Import, Resolved, 1))
    );
    // A module only a macro could declare.
    let (_, reason, detail) = unresolved(
        &g,
        "crates/core-model/src/lib.rs",
        "crate::generated::Table",
    )
    .unwrap();
    assert_eq!(reason, "not_found");
    assert!(detail.contains("generated by a macro"), "{detail}");
    // Externals: workspace dependency and std.
    assert_eq!(
        edge(&g, &core("graph/node.rs"), "external:crates:serde"),
        Some((ImportExternal, Resolved, 1))
    );
    assert_eq!(
        edge(&g, &core("graph/edge.rs"), "external:rust:std"),
        Some((ImportExternal, Exact, 1))
    );
    // A file no `mod` reaches joins its crate by location, heuristically.
    assert_eq!(
        edge(&g, &core("orphan.rs"), &core("graph.rs")),
        Some((Import, Heuristic, 1))
    );

    // Workspace crates from the app: workspace = true, renamed path dependency, re-exports.
    let main = "file:crates/app/src/main.rs";
    assert_eq!(
        edge(&g, main, &core("graph/node.rs")),
        Some((Import, Resolved, 1))
    );
    assert_eq!(
        site(&g, "crates/app/src/main.rs", "core_model::Store"),
        Some((4, lib.as_str()))
    );
    assert_eq!(
        site(&g, "crates/app/src/main.rs", "core_model::ids::NodeId"),
        Some((5, core("gen/ids.rs").as_str()))
    );
    assert_eq!(
        edge(&g, main, "file:crates/local-util/src/helpers.rs"),
        Some((Import, Resolved, 1))
    );
    assert_eq!(
        edge(&g, main, "external:crates:anyhow"),
        Some((ImportExternal, Resolved, 1))
    );
    assert_eq!(
        edge(&g, main, "external:rust:alloc"),
        Some((ImportExternal, Exact, 1))
    );
    assert_eq!(
        unresolved(&g, "crates/app/src/main.rs", "nonexistent_crate::X").map(|u| u.1),
        Some("not_found")
    );
    // `use` inside a function body.
    assert_eq!(
        site(&g, "crates/app/src/main.rs", "crate::cli::args::Args"),
        Some((12, "file:crates/app/src/cli/args.rs"))
    );
    // Integration tests and bins are crates of their own; the bin has no lib to import.
    assert_eq!(
        edge(
            &g,
            "file:crates/app/tests/smoke.rs",
            "file:crates/app/tests/common/mod.rs"
        ),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(
            &g,
            "file:crates/app/tests/smoke.rs",
            "external:crates:pretty_assertions"
        ),
        Some((ImportExternal, Resolved, 1))
    );
    assert_eq!(
        edge(&g, "file:crates/app/build.rs", "external:rust:std"),
        Some((ImportExternal, Exact, 1))
    );
    assert_eq!(
        unresolved(&g, "crates/app/src/bin/tool.rs", "app_cli::cli").map(|u| u.1),
        Some("not_found")
    );
    // A broken `use` is a parse error, not a silent drop.
    assert!(
        g.unresolved
            .iter()
            .any(|u| u.file == "crates/app/src/broken.rs" && u.reason.as_str() == "parse_error"),
        "{:?}",
        g.unresolved
    );
    // CRLF keeps line numbers.
    assert_eq!(
        site(
            &g,
            "crates/local-util/src/crlf/mod.rs",
            "crate::helpers::help"
        ),
        Some((3, "file:crates/local-util/src/helpers.rs"))
    );
    // Package dependencies follow the crate edges.
    assert!(
        edge(
            &g,
            "package:crates/app/Cargo.toml",
            "package:crates/core-model/Cargo.toml"
        )
        .is_some_and(|e| e.0 == EdgeKind::DependsOn)
    );
}

#[test]
fn c_without_compile_commands_golden() {
    let g = index_fixture("c-plain");
    check_golden("c-plain", &g);
    let main = "src/main.c";
    let m = "file:src/main.c";
    assert_eq!(edge(&g, m, "file:src/util.h"), Some((Import, Heuristic, 1)));
    assert_eq!(
        edge(&g, m, "file:include/net/socket.h"),
        Some((Import, Heuristic, 1))
    );
    assert_eq!(
        edge(&g, m, "external:c:stdio.h"),
        Some((ImportExternal, Exact, 1))
    );
    assert_eq!(
        site(&g, main, "<proj/config.h>"),
        Some((4, "file:include/proj/config.h"))
    );
    assert_eq!(
        site(&g, main, "../include/proj/config.h"),
        Some((10, "file:include/proj/config.h"))
    );
    assert_eq!(
        unresolved(&g, main, "missing.h").map(|u| u.1),
        Some("not_found")
    );
    assert_eq!(
        unresolved(&g, main, "CONFIG_H").map(|u| u.1),
        Some("dynamic")
    );
    assert_eq!(
        edge(&g, m, "external:system:zlib.h"),
        Some((ImportExternal, Resolved, 1))
    );
    let (_, reason, detail) = unresolved(&g, main, "dup.h").unwrap();
    assert_eq!(reason, "ambiguous");
    assert!(detail.starts_with("2 candidates"), "{detail}");
    assert_eq!(
        edge(&g, m, "file:third_party/x/only_once.h"),
        Some((Import, Heuristic, 1))
    );
    // Two include/ dirs have common.h: the includer's own project wins.
    assert_eq!(
        edge(
            &g,
            "file:libs/foo/src/foo.c",
            "file:libs/foo/include/common.h"
        ),
        Some((Import, Heuristic, 1))
    );
    assert_eq!(
        edge(&g, "file:include/net/socket.h", "external:system:sys"),
        Some((ImportExternal, Resolved, 1))
    );
}

#[test]
fn c_with_compile_commands_golden() {
    let g = index_fixture("c-compdb");
    check_golden("c-compdb", &g);
    let m = "file:src/main.c";
    // -I / -iquote / -isystem of the file's own entry: exact, even with a lookalike.
    assert_eq!(edge(&g, m, "file:include/api.h"), Some((Import, Exact, 1)));
    assert_eq!(edge(&g, m, "file:src/quoted/q.h"), Some((Import, Exact, 1)));
    assert_eq!(
        edge(&g, m, "file:third_party/sys/s.h"),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(&g, "file:src/other.c", "file:other/api.h"),
        Some((Import, Exact, 1))
    );
    // A header has no entry: the includer's dir, then the union of every entry's dirs.
    assert_eq!(
        edge(&g, "file:include/api.h", "file:include/detail.h"),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(&g, "file:include/api.h", "file:src/quoted/q.h"),
        Some((Import, Exact, 1))
    );
    let (_, reason, detail) = unresolved(&g, "src/main.c", "gen/version.h").unwrap();
    assert_eq!(reason, "not_found");
    assert!(detail.contains("build/compile_commands.json"), "{detail}");
    assert_eq!(
        edge(&g, m, "external:c:stdlib.h"),
        Some((ImportExternal, Exact, 1))
    );
    assert_eq!(
        edge(&g, m, "external:system:openssl"),
        Some((ImportExternal, Resolved, 1))
    );
    assert!(g.diagnostics.is_empty(), "{:?}", g.diagnostics);
}

/// The same database with absolute paths (native separators) and the `command` form
/// gives the same graph.
#[test]
fn compile_commands_with_absolute_paths_and_command_strings() {
    let (_tmp, root) = copy_fixture("c-compdb");
    let abs = |p: &str| root.join(p).display().to_string();
    let quote = |s: String| format!("\"{}\"", s.replace('\\', "\\\\"));
    let db = json!([
        {
            "directory": abs("build"),
            "file": abs("src/main.c"),
            "command": format!(
                "cc -I{} -iquote {} -isystem {} -c main.c",
                quote(abs("include")),
                quote(abs("src/quoted")),
                quote(abs("third_party/sys")),
            ),
        },
        {
            "directory": root.display().to_string(),
            "file": "src/other.c",
            "arguments": ["cl.exe", "/I", "other", "/c", "src/other.c"],
        }
    ]);
    fs::write(
        root.join("build/compile_commands.json"),
        serde_json::to_string_pretty(&db).unwrap(),
    )
    .unwrap();
    let g = index(&root, &IndexOptions::default()).unwrap().graph;
    assert_eq!(golden_view(&g), golden_view(&index_fixture("c-compdb")));
}

/// Without the database the same project falls back to heuristics; a broken database is
/// a diagnostic, never a silent switch.
#[test]
fn compile_commands_missing_or_broken() {
    let (_tmp, root) = copy_fixture("c-compdb");
    fs::remove_file(root.join("build/compile_commands.json")).unwrap();
    let g = index(&root, &IndexOptions::default()).unwrap().graph;
    assert_eq!(
        edge(&g, "file:src/main.c", "file:include/api.h"),
        Some((Import, Heuristic, 1))
    );
    // The guess differs from what the compiler does (`-I other`): that is why it is
    // heuristic.
    assert_eq!(
        edge(&g, "file:src/other.c", "file:include/api.h"),
        Some((Import, Heuristic, 1))
    );

    fs::write(root.join("compile_commands.json"), "{ not json").unwrap();
    let g = index(&root, &IndexOptions::default()).unwrap().graph;
    assert!(
        g.diagnostics
            .iter()
            .any(|d| d.path == "compile_commands.json" && d.detail.contains("not valid JSON")),
        "{:?}",
        g.diagnostics
    );
    assert_eq!(
        edge(&g, "file:src/main.c", "file:include/api.h"),
        Some((Import, Heuristic, 1))
    );
}

/// Compilation databases are often larger than the 1 MB file limit: still used.
#[test]
fn compile_commands_larger_than_the_file_limit() {
    let (_tmp, root) = copy_fixture("c-compdb");
    let db_path = root.join("build/compile_commands.json");
    let mut db: Value = serde_json::from_str(&fs::read_to_string(&db_path).unwrap()).unwrap();
    let pad = format!("-DPAD={}", "x".repeat(64 * 1024));
    let filler = json!({
        "directory": "..",
        "file": "src/generated_filler.c",
        "arguments": ["cc", pad, "-c", "x.c"],
    });
    for _ in 0..20 {
        db.as_array_mut().unwrap().push(filler.clone());
    }
    let text = serde_json::to_string(&db).unwrap();
    assert!(text.len() as u64 > cartograph_index::walk::DEFAULT_MAX_FILE_BYTES);
    fs::write(&db_path, text).unwrap();
    let g = index(&root, &IndexOptions::default()).unwrap().graph;
    assert_eq!(
        edge(&g, "file:src/main.c", "file:include/api.h"),
        Some((Import, Exact, 1))
    );
    assert!(g.diagnostics.is_empty(), "{:?}", g.diagnostics);
}

#[test]
fn cpp_headers_in_include_golden() {
    let g = index_fixture("cpp-include");
    check_golden("cpp-include", &g);
    let e = "file:src/engine.cpp";
    assert_eq!(
        edge(&g, e, "file:include/engine/engine.hpp"),
        Some((Import, Heuristic, 1))
    );
    assert_eq!(
        edge(&g, e, "file:src/detail/impl.hpp"),
        Some((Import, Heuristic, 1))
    );
    assert_eq!(
        edge(&g, e, "file:include/engine/pool.h"),
        Some((Import, Heuristic, 1))
    );
    assert_eq!(
        edge(&g, e, "external:cpp:vector"),
        Some((ImportExternal, Exact, 1))
    );
    // A C++ header with a `.h` name is parsed as C: its includes are still found and no
    // parse error is invented.
    assert_eq!(
        edge(
            &g,
            "file:include/engine/pool.h",
            "file:include/engine/fwd.hpp"
        ),
        Some((Import, Heuristic, 1))
    );
    assert!(
        !g.unresolved
            .iter()
            .any(|u| u.reason.as_str() == "parse_error"),
        "{:?}",
        g.unresolved
    );
    assert_eq!(
        edge(&g, "file:tests/engine_test.cc", "file:src/detail/impl.hpp"),
        Some((Import, Heuristic, 1))
    );
    assert_eq!(
        edge(&g, "file:tests/engine_test.cc", "external:system:gtest"),
        Some((ImportExternal, Resolved, 1))
    );
}
