//! Golden tests for N1 import resolution.
//!
//! Each fixture under `tests/fixtures/imports-*` is indexed from a temp copy and its import
//! edges, derived edges, import sites, unresolved imports, externals and config
//! diagnostics are compared exactly (sorted) with `tests/golden/<fixture>.json`.
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test imports_golden`
//! and review the diff.
//!
//! A few hand-written assertions per fixture pin the cases the golden file must keep, so a
//! regenerated golden cannot silently accept a regression in them.

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

/// Index a temp copy of `tests/fixtures/<name>`.
fn index_fixture(name: &str) -> Graph {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    copy_dir(&repo_root().join("tests/fixtures").join(name), &root);
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

fn unresolved<'g>(g: &'g Graph, file: &str, spec: &str) -> Option<(u32, &'g str)> {
    g.unresolved
        .iter()
        .find(|u| u.file == file && u.specifier == spec)
        .map(|u| (u.line, u.reason.as_str()))
}

#[test]
fn crlf_fixture_files_kept_their_line_endings() {
    for f in [
        "tests/fixtures/imports-ts/apps/web/src/crlf/win.ts",
        "tests/fixtures/imports-py/src/fxpy/crlf/win.py",
    ] {
        let bytes = fs::read(repo_root().join(f)).unwrap();
        assert!(
            bytes.windows(2).any(|w| w == b"\r\n"),
            "{f} lost its CRLF line endings"
        );
    }
}

#[test]
fn imports_ts_golden() {
    use Confidence::*;
    use EdgeKind::*;
    let g = index_fixture("imports-ts");
    let web = "file:apps/web/src";
    // `import type { Props } from './types'` is an Import edge like any other, marked
    // type-only in the importer's meta; runtime imports are not listed there.
    let app = g.node(&format!("{web}/App.tsx")).unwrap();
    assert_eq!(
        app.meta["type_only_imports"],
        serde_json::json!([format!("{web}/types.ts")])
    );
    assert_eq!(
        edge(&g, &format!("{web}/App.tsx"), &format!("{web}/types.ts")),
        Some((Import, Exact, 1))
    );
    // Relative with extension probing, index resolution, .js -> .ts, re-exports.
    assert_eq!(
        edge(&g, &format!("{web}/main.tsx"), &format!("{web}/App.tsx")),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(
            &g,
            &format!("{web}/App.tsx"),
            &format!("{web}/util/index.ts")
        ),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(&g, &format!("{web}/App.tsx"), &format!("{web}/helper.ts")),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(&g, &format!("{web}/App.tsx"), &format!("{web}/reexport.ts")),
        Some((Import, Exact, 1))
    );
    // tsconfig paths inherited through `extends` (without .json); a paths target can be a
    // non-code file.
    assert_eq!(
        edge(
            &g,
            &format!("{web}/main.tsx"),
            "file:packages/shared/src/math.ts"
        ),
        Some((Import, Resolved, 1))
    );
    assert_eq!(
        edge(&g, &format!("{web}/main.tsx"), &format!("{web}/styles.css")),
        Some((Import, Resolved, 1))
    );
    // Paths from the second entry of an `extends` array.
    assert_eq!(
        edge(
            &g,
            "file:packages/ui/src/index.ts",
            "file:packages/ui/src/Card.tsx"
        ),
        Some((Import, Resolved, 1))
    );
    // Workspace packages: exports "." conditions, exports subpath, main missing -> src/index.
    assert_eq!(
        edge(
            &g,
            &format!("{web}/main.tsx"),
            "file:packages/ui/src/index.ts"
        ),
        Some((Import, Resolved, 1))
    );
    assert_eq!(
        edge(
            &g,
            &format!("{web}/main.tsx"),
            "file:packages/ui/src/Card.tsx"
        ),
        Some((Import, Resolved, 1))
    );
    assert_eq!(
        edge(
            &g,
            "file:packages/ui/src/Button.tsx",
            "file:packages/shared/src/index.ts"
        ),
        Some((Import, Resolved, 1))
    );
    // require / dynamic import with literals; template literal without substitution.
    assert_eq!(
        edge(
            &g,
            &format!("{web}/loader.js"),
            &format!("{web}/cjs-mod.cjs")
        ),
        Some((Import, Exact, 2))
    );
    assert_eq!(
        edge(&g, &format!("{web}/loader.js"), &format!("{web}/lazy.mjs")),
        Some((Import, Exact, 1))
    );
    // Externals.
    for (src, ext, conf) in [
        ("main.tsx", "external:npm:react", Resolved),
        ("main.tsx", "external:node:fs", Exact),
        ("main.tsx", "external:node:path", Exact),
        ("main.tsx", "external:npm:lodash", Resolved),
        ("main.tsx", "external:npm:@scope/pkg", Resolved),
        ("loader.js", "external:npm:express", Resolved),
        // Runtime schemes (found by a bench on a Cloudflare Worker: `cloudflare:sockets`).
        ("runtime.ts", "external:cloudflare:sockets", Exact),
        ("runtime.ts", "external:bun:sqlite", Exact),
        ("runtime.ts", "external:npm:react", Resolved),
    ] {
        assert_eq!(
            edge(&g, &format!("{web}/{src}"), ext),
            Some((ImportExternal, conf, 1)),
            "{src} -> {ext}"
        );
    }
    // The documented guess: undeclared bare specifier matching a local file.
    assert_eq!(
        edge(&g, "file:scripts/tool.ts", "file:config/app.ts"),
        Some((Import, Heuristic, 1))
    );
    // CRLF file: lines are counted on \n.
    let crlf: Vec<(u32, &str)> = g
        .imports
        .iter()
        .filter(|s| s.file == "apps/web/src/crlf/win.ts")
        .map(|s| (s.line, s.specifier.as_str()))
        .collect();
    assert_eq!(
        crlf,
        vec![
            (2, "../../../../packages/shared/src/math"),
            (4, "../helper.js")
        ]
    );
    // Unresolved, every reason.
    let main = "apps/web/src/main.tsx";
    assert_eq!(
        unresolved(&g, main, "@missing/thing"),
        Some((9, "not_found"))
    );
    assert_eq!(unresolved(&g, main, "@fx/dup"), Some((12, "ambiguous")));
    assert_eq!(unresolved(&g, main, "#internal"), Some((13, "not_found")));
    assert_eq!(unresolved(&g, main, "~/nowhere"), Some((14, "not_found")));
    let app = "apps/web/src/App.tsx";
    assert_eq!(unresolved(&g, app, "./missing"), Some((5, "not_found")));
    let loader = "apps/web/src/loader.js";
    assert_eq!(unresolved(&g, loader, "moduleName"), Some((2, "dynamic")));
    assert_eq!(
        unresolved(&g, loader, "`./locale/${lang}.js`"),
        Some((5, "dynamic"))
    );
    assert!(
        g.unresolved
            .iter()
            .any(|u| u.file == "apps/web/src/broken.ts" && u.reason.as_str() == "parse_error"),
        "{:?}",
        g.unresolved
    );
    // Missing `extends` entry is recorded, not dropped.
    assert!(
        g.diagnostics
            .iter()
            .any(|d| d.path == "apps/web/tsconfig.json" && d.detail.contains("nope.json"))
    );
    // Aggregates.
    assert_eq!(
        edge(
            &g,
            "package:apps/web/package.json",
            "package:packages/ui/package.json"
        )
        .map(|e| e.0),
        Some(DependsOn)
    );
    assert!(g.edges.iter().any(|e| e.kind == Contains
        && e.src == "package:packages/ui/package.json"
        && e.dst == "dir:packages/ui"));
    for e in g.edges.iter().filter(|e| e.kind == DependsOn) {
        assert_ne!(e.src, e.dst, "self-loop {e:?}");
    }
    check_golden("imports-ts", &g);
}

#[test]
fn imports_py_golden() {
    use Confidence::*;
    use EdgeKind::*;
    let g = index_fixture("imports-py");
    let pkg = "file:src/fxpy";
    // Relative imports, `from . import x` as a submodule, `__init__` targets.
    assert_eq!(
        edge(&g, &format!("{pkg}/__init__.py"), &format!("{pkg}/core.py")),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(
            &g,
            &format!("{pkg}/__init__.py"),
            &format!("{pkg}/utils/__init__.py")
        ),
        Some((Import, Exact, 1))
    );
    assert_eq!(
        edge(
            &g,
            &format!("{pkg}/utils/helpers.py"),
            &format!("{pkg}/utils/text.py")
        ),
        // `from . import text` (Exact) and importlib.import_module("fxpy.utils.text").
        Some((Import, Exact, 2))
    );
    assert_eq!(
        edge(
            &g,
            &format!("{pkg}/utils/helpers.py"),
            &format!("{pkg}/models.py")
        ),
        Some((Import, Exact, 1))
    );
    // Absolute imports against the src/ root, including from tests/ next to src/.
    assert_eq!(
        edge(
            &g,
            &format!("{pkg}/core.py"),
            &format!("{pkg}/utils/helpers.py")
        ),
        Some((Import, Resolved, 1))
    );
    assert_eq!(
        edge(&g, "file:tests/test_core.py", &format!("{pkg}/core.py")),
        Some((Import, Resolved, 1))
    );
    // Import inside a function (attribute of the package -> __init__), namespace package.
    assert_eq!(
        edge(&g, &format!("{pkg}/core.py"), &format!("{pkg}/__init__.py")),
        Some((Import, Resolved, 1))
    );
    assert_eq!(
        edge(&g, &format!("{pkg}/core.py"), "dir:src/fxpy/ns"),
        Some((Import, Resolved, 2))
    );
    // Stdlib, third party (also inside a function and in try/except).
    for (src, ext, conf) in [
        ("core.py", "external:python:os", Exact),
        ("core.py", "external:python:json", Exact),
        ("core.py", "external:python:typing", Exact),
        ("core.py", "external:pypi:requests", Resolved),
        ("core.py", "external:pypi:numpy", Resolved),
        ("core.py", "external:pypi:ujson", Resolved),
        ("models.py", "external:python:__future__", Exact),
    ] {
        assert_eq!(
            edge(&g, &format!("{pkg}/{src}"), ext).map(|e| (e.0, e.1)),
            Some((ImportExternal, conf)),
            "{src} -> {ext}"
        );
    }
    // PEP 420: bare top-level folders (no __init__.py) do not shadow installed packages,
    // even from inside the folder itself; a namespace package that the import lands on a
    // file in is local.
    for (src, ext) in [
        ("file:tests/test_tools.py", "external:pypi:alembic"),
        ("file:tests/test_tools.py", "external:pypi:docker"),
        ("file:alembic/env.py", "external:pypi:alembic"),
    ] {
        assert_eq!(
            edge(&g, src, ext).map(|e| (e.0, e.1)),
            Some((ImportExternal, Resolved)),
            "{src} -> {ext}"
        );
    }
    assert_eq!(
        edge(&g, "file:tests/test_tools.py", "file:plugins/extra.py"),
        Some((Import, Resolved, 1))
    );
    // The documented guess: a script importing a sibling module.
    assert_eq!(
        edge(&g, "file:scripts/run.py", "file:scripts/helperlib.py"),
        Some((Import, Heuristic, 1))
    );
    assert_eq!(
        edge(&g, "file:scripts/run.py", &format!("{pkg}/__init__.py")),
        Some((Import, Resolved, 1))
    );
    // CRLF.
    let crlf: Vec<(u32, &str)> = g
        .imports
        .iter()
        .filter(|s| s.file == "src/fxpy/crlf/win.py")
        .map(|s| (s.line, s.specifier.as_str()))
        .collect();
    assert_eq!(crlf, vec![(2, "fxpy.core"), (4, "sys")]);
    // Unresolved.
    assert_eq!(
        unresolved(&g, "src/fxpy/core.py", "..outside"),
        Some((7, "not_found"))
    );
    assert_eq!(
        unresolved(&g, "tests/test_core.py", "fxpy.nothere"),
        Some((3, "not_found"))
    );
    assert_eq!(
        unresolved(&g, "src/fxpy/utils/helpers.py", "name"),
        Some((7, "dynamic"))
    );
    assert!(
        g.unresolved
            .iter()
            .any(|u| u.file == "src/fxpy/broken.py" && u.reason.as_str() == "parse_error"),
        "{:?}",
        g.unresolved
    );
    check_golden("imports-py", &g);
}

#[test]
fn report_sections_on_imports_ts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("imports-ts");
    copy_dir(&repo_root().join("tests/fixtures/imports-ts"), &root);
    let opts = IndexOptions {
        data_dir: Some(tmp.path().join("db")),
        ..IndexOptions::default()
    };
    cartograph_index::index_to_store(&root, &opts).unwrap();
    let store = cartograph_core::Store::open(tmp.path().join("db/graph.db")).unwrap();
    let md = cartograph_index::report::render(&store).unwrap();
    for needle in [
        "## Imports",
        "42 imports: 25 resolved to files (59.5%), 9 external (21.4%), 8 unresolved (19.0%).",
        "| TS/JS | 42 | 25 (59.5%) | 9 (21.4%) | 8 (19.0%) | 81.0% |",
        "File imports by confidence: exact 16, resolved 8, heuristic 1.",
        "### Unresolved imports (8)",
        "| not_found | 4 |",
        "| ambiguous | `@fx/dup` | 1 | apps/web/src/main.tsx:12 |",
        "### Top external packages (8 distinct)",
        "| npm:react | 2 | 2 |",
        "### Top files by fan-in",
        "| packages/shared/src/math.ts | 6 |",
        "### Top files by fan-out",
        "| apps/web/src/main.tsx | 5 |",
        "### Top directory dependencies",
        "| apps/web/src | packages/shared/src | 3 |",
        "| resolve imports |",
    ] {
        assert!(md.contains(needle), "missing {needle:?} in report:\n{md}");
    }
}
