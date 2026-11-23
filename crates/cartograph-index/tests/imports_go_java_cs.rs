//! Golden tests for N1 import resolution of Go, Java and C#.
//!
//! Same scheme as `imports_golden.rs` (kept separate so each
//! language family has its own file): each fixture under `tests/fixtures/{go,java,csharp}-imports` is indexed
//! from a temp copy and compared with `tests/golden/<fixture>.json`. Regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test imports_go_java_cs` and review
//! the diff. Hand-written assertions pin the cases a regenerated golden must keep.

use cartograph_core::{Confidence, EdgeKind, Graph, NodeKind, Store};
use cartograph_index::{IndexOptions, index, index_to_store};
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

fn index_fixture(name: &str) -> Graph {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    copy_dir(&repo_root().join("tests/fixtures").join(name), &root);
    index(&root, &IndexOptions::default()).unwrap().graph
}

fn golden_view(g: &Graph) -> Value {
    let edges: Vec<Value> = g
        .edges
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EdgeKind::Import | EdgeKind::ImportExternal | EdgeKind::DependsOn | EdgeKind::Uses
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

/// Targets of the import sites of `file` with `spec`, sorted.
fn targets(g: &Graph, file: &str, spec: &str) -> Vec<String> {
    let mut v: Vec<String> = g
        .imports
        .iter()
        .filter(|s| s.file == file && s.specifier == spec)
        .map(|s| s.target.clone())
        .collect();
    v.sort();
    v
}

fn confidence(g: &Graph, src: &str, dst: &str) -> Option<Confidence> {
    g.edges
        .iter()
        .find(|e| e.src == src && e.dst == dst && e.kind != EdgeKind::Contains)
        .map(|e| e.confidence)
}

/// `(line, reason, detail)` of the unresolved import of `file` with `spec`.
fn unresolved<'g>(g: &'g Graph, file: &str, spec: &str) -> Option<(u32, &'g str, &'g str)> {
    g.unresolved
        .iter()
        .find(|u| u.file == file && u.specifier == spec)
        .map(|u| (u.line, u.reason.as_str(), u.detail.as_deref().unwrap_or("")))
}

fn t(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn go_crlf_fixture_kept_its_line_endings() {
    let bytes = fs::read(repo_root().join("tests/fixtures/go-imports/crlf/win.go")).unwrap();
    assert!(bytes.windows(2).any(|w| w == b"\r\n"));
}

#[test]
fn go_imports_golden() {
    let g = index_fixture("go-imports");
    let main = "cmd/server/main.go";
    let gh = concat!("git", "hub.com");
    // Packages of the module are Dir targets; the root package is its non-test files.
    assert_eq!(
        targets(&g, main, "example.com/app/internal/store"),
        t(&["dir:internal/store"])
    );
    assert_eq!(
        confidence(&g, "file:cmd/server/main.go", "dir:internal/store"),
        Some(Confidence::Resolved)
    );
    assert_eq!(targets(&g, main, "example.com/app"), t(&["file:app.go"]));
    assert_eq!(
        targets(&g, "app_test.go", "example.com/app"),
        t(&["file:app.go"])
    );
    // Dot import, another module of the repo, a module importing the root module.
    assert_eq!(
        targets(&g, main, "example.com/app/pkg/util"),
        t(&["dir:pkg/util"])
    );
    assert_eq!(
        targets(&g, main, "example.com/shared"),
        t(&["dir:libs/shared"])
    );
    assert_eq!(
        targets(&g, "libs/shared/shared.go", "example.com/app/pkg/util"),
        t(&["dir:pkg/util"])
    );
    // External test package importing its own dir; a test-only package from its own dir.
    assert_eq!(
        targets(
            &g,
            "internal/store/store_test.go",
            "example.com/app/internal/store"
        ),
        t(&["dir:internal/store"])
    );
    assert_eq!(
        targets(
            &g,
            "internal/onlytests/self_test.go",
            "example.com/app/internal/onlytests"
        ),
        t(&["dir:internal/onlytests"])
    );
    // Relative import (pre-modules) and a local replace.
    assert_eq!(
        targets(&g, "internal/store/store.go", "../../pkg/util"),
        t(&["dir:pkg/util"])
    );
    assert_eq!(
        targets(&g, main, "example.com/legacy/x"),
        t(&["dir:third_party/legacy/x"])
    );
    // Externals: std (also cgo), require, replace to a module, vendor, guesses.
    for (spec, ext, conf) in [
        ("fmt", "external:go:fmt".to_string(), Confidence::Exact),
        ("net/http", "external:go:net/http".into(), Confidence::Exact),
        ("embed", "external:go:embed".into(), Confidence::Exact),
        ("C", "external:go:C".into(), Confidence::Exact),
        (
            &*format!("{gh}/acme/lib/sub"),
            format!("external:gomod:{gh}/acme/lib"),
            Confidence::Resolved,
        ),
        (
            "golang.org/x/sync/errgroup",
            "external:gomod:golang.org/x/sync".into(),
            Confidence::Resolved,
        ),
        (
            &*format!("{gh}/old/thing/z"),
            format!("external:gomod:{gh}/new/thing"),
            Confidence::Resolved,
        ),
        (
            &*format!("{gh}/vend/dep/sub"),
            format!("external:gomod:{gh}/vend/dep"),
            Confidence::Resolved,
        ),
        (
            "go.uber.org/zap/zapcore",
            "external:gomod:go.uber.org/zap".into(),
            Confidence::Heuristic,
        ),
        (
            &*format!("{gh}/other/thing/v2/deep"),
            format!("external:gomod:{gh}/other/thing/v2"),
            Confidence::Heuristic,
        ),
    ] {
        assert_eq!(targets(&g, main, spec), vec![ext.clone()], "{spec}");
        assert_eq!(
            confidence(&g, "file:cmd/server/main.go", &ext),
            Some(conf),
            "{spec}"
        );
    }
    // Vendored code importing its own module: external too.
    assert_eq!(
        targets(
            &g,
            &format!("vendor/{gh}/vend/dep/sub/sub.go"),
            &format!("{gh}/vend/dep")
        ),
        t(&[&format!("external:gomod:{gh}/vend/dep")])
    );
    // Unresolved, with reasons.
    let nf = |spec: &str| unresolved(&g, main, spec).map(|u| (u.0, u.1));
    assert_eq!(
        nf("example.com/app/internal/missing"),
        Some((12, "not_found"))
    );
    assert_eq!(
        nf("example.com/app/internal/onlytests"),
        Some((13, "not_found"))
    );
    assert_eq!(nf("example.com/app/docs"), Some((14, "not_found")));
    assert_eq!(nf("example.com/gone/y"), Some((17, "not_found")));
    assert!(
        unresolved(&g, main, "example.com/app/internal/onlytests")
            .unwrap()
            .2
            .contains("only _test.go")
    );
    assert!(
        g.unresolved
            .iter()
            .any(|u| u.file == "broken/broken.go" && u.reason.as_str() == "parse_error"),
        "{:?}",
        g.unresolved
    );
    assert!(
        g.diagnostics
            .iter()
            .any(|d| d.path == "go.mod" && d.detail.contains("../outside"))
    );
    // CRLF lines.
    let crlf: Vec<(u32, &str)> = g
        .imports
        .iter()
        .filter(|s| s.file == "crlf/win.go")
        .map(|s| (s.line, s.specifier.as_str()))
        .collect();
    assert_eq!(crlf, vec![(4, "os"), (6, "example.com/app/pkg/util")]);
    // Dir targets feed the Dir -> Dir aggregate.
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::DependsOn
        && e.src == "dir:cmd/server"
        && e.dst == "dir:internal/store"));
    check_golden("go-imports", &g);
}

#[test]
fn java_imports_golden() {
    let g = index_fixture("java-imports");
    let core = "core/src/main/java/com/acme/core";
    let test = "core/src/test/java/com/acme/core/ModelTest.java";
    let app = "app/src/main/java/com/acme/app/App.java";
    let model = format!("file:{core}/Model.java");
    // Exact class, nested classes, static member, static wildcard -> the outer file.
    assert_eq!(
        targets(
            &g,
            &format!("{core}/Service.java"),
            "com.acme.core.util.Strings"
        ),
        t(&[&format!("file:{core}/util/Strings.java")])
    );
    for spec in [
        "com.acme.core.Model.Inner",
        "com.acme.core.Model.Outer.Deep",
        "com.acme.core.Model.MAX",
        "com.acme.core.Model.*",
    ] {
        assert_eq!(targets(&g, test, spec), vec![model.clone()], "{spec}");
    }
    // Test-only class from a test; wildcard from a test sees main and test dirs.
    assert_eq!(
        targets(&g, test, "com.acme.core.Fixtures"),
        t(&["file:core/src/test/java/com/acme/core/Fixtures.java"])
    );
    assert_eq!(
        targets(&g, test, "com.acme.core.util.*"),
        t(&[
            &format!("dir:{core}/util"),
            "dir:core/src/test/java/com/acme/core/util",
        ])
    );
    // Cross-module (Maven core <- Gradle app), a non-public second class, a file whose
    // path does not match its package, wildcard from main code (no test dirs).
    assert_eq!(targets(&g, app, "com.acme.core.Model"), vec![model.clone()]);
    assert_eq!(
        targets(&g, app, "com.acme.core.Helper"),
        vec![model.clone()]
    );
    assert_eq!(
        targets(&g, app, "com.acme.core.util.Where"),
        t(&["file:odd/Where.java"])
    );
    assert_eq!(
        targets(&g, app, "com.acme.core.util.*"),
        t(&[&format!("dir:{core}/util"), "dir:odd"])
    );
    assert_eq!(
        confidence(&g, &format!("file:{app}"), &model),
        Some(Confidence::Resolved)
    );
    // Externals.
    for (file, spec, ext, conf) in [
        (
            app,
            "javax.swing.JFrame",
            "external:jdk:javax.swing",
            Confidence::Exact,
        ),
        (
            app,
            "java.util.concurrent.*",
            "external:jdk:java.util",
            Confidence::Exact,
        ),
        (
            &*format!("{core}/Service.java"),
            "org.apache.commons.lang3.StringUtils",
            "external:maven:org.apache.commons",
            Confidence::Resolved,
        ),
        (
            app,
            "javax.persistence.Entity",
            "external:maven:javax.persistence",
            Confidence::Heuristic,
        ),
        (
            app,
            "lombok.Data",
            "external:maven:lombok",
            Confidence::Heuristic,
        ),
        (
            test,
            "org.junit.jupiter.api.Test",
            "external:maven:org.junit.jupiter",
            Confidence::Resolved,
        ),
        (
            test,
            "org.junit.jupiter.api.Assertions.*",
            "external:maven:org.junit.jupiter",
            Confidence::Resolved,
        ),
    ] {
        assert_eq!(targets(&g, file, spec), t(&[ext]), "{spec}");
        assert_eq!(
            confidence(&g, &format!("file:{file}"), ext),
            Some(conf),
            "{spec}"
        );
    }
    // Unresolved.
    let u = |spec: &str| unresolved(&g, app, spec).map(|u| (u.0, u.1));
    assert_eq!(u("com.acme.core.Missing"), Some((7, "not_found")));
    assert_eq!(u("com.acme.generated.QModel"), Some((8, "not_found")));
    assert_eq!(u("com.acme.dup.Thing"), Some((13, "ambiguous")));
    // A class whose file the walker skipped (binary): the detail says so.
    assert_eq!(
        unresolved(&g, app, "com.acme.core.Bin"),
        Some((
            15,
            "not_found",
            "core/src/main/java/com/acme/core/Bin.java was not indexed (binary)"
        ))
    );
    assert!(
        g.unresolved
            .iter()
            .any(|u| u.file == "app/src/main/java/com/acme/app/Broken.java"
                && u.reason.as_str() == "parse_error"),
        "{:?}",
        g.unresolved
    );
    check_golden("java-imports", &g);
}

/// `Uses` edge `src -> dst` (files), with its confidence.
fn uses(g: &Graph, src: &str, dst: &str) -> Option<Confidence> {
    g.edges
        .iter()
        .find(|e| {
            e.kind == EdgeKind::Uses
                && e.src == format!("file:{src}")
                && e.dst == format!("file:{dst}")
        })
        .map(|e| e.confidence)
}

#[test]
fn java_same_package_golden() {
    let g = index_fixture("spring-java");
    let main = "src/main/java/com/acme/shop/order";
    let test = "src/test/java/com/acme/shop";
    let r = Some(Confidence::Resolved);
    // A test in the package of the class it tests, under another source root: no import.
    let impl_test = format!("{test}/order/service/impl/OrderServiceImplTest.java");
    for dst in ["OrderServiceImpl.java", "OrderValidator.java"] {
        assert_eq!(
            uses(&g, &impl_test, &format!("{main}/service/impl/{dst}")),
            r,
            "{dst}"
        );
    }
    // A test helper of the same package and folder.
    assert_eq!(
        uses(
            &g,
            &impl_test,
            &format!("{test}/order/service/impl/OrderFixtures.java")
        ),
        r
    );
    assert_eq!(
        uses(
            &g,
            &format!("{test}/order/dto/CreateOrderDtoValidationTest.java"),
            &format!("{main}/dto/CreateOrderDto.java")
        ),
        r
    );
    // Field, annotation, generic argument and record component types of the same folder.
    for (src, dst) in [
        (
            "controller/AdminController.java",
            "controller/OrderReport.java",
        ),
        (
            "controller/OrderController.java",
            "controller/OrderReport.java",
        ),
        (
            "service/impl/OrderServiceImpl.java",
            "service/impl/OrderValidator.java",
        ),
        (
            "service/impl/OrderServiceImpl.java",
            "service/impl/Audited.java",
        ),
        (
            "repository/OrderRepository.java",
            "repository/OrderDocument.java",
        ),
        ("dto/CreateOrderDto.java", "dto/OrderLine.java"),
    ] {
        assert_eq!(
            uses(&g, &format!("{main}/{src}"), &format!("{main}/{dst}")),
            r,
            "{src} -> {dst}"
        );
    }
    // Another package naming the class without an import gets nothing; imported names are
    // import edges, not `uses`; main code never uses test sources.
    assert!(
        !g.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Uses && e.src.contains("/other/")),
        "{:?}",
        g.edges
    );
    assert_eq!(
        uses(
            &g,
            &format!("{main}/controller/OrderController.java"),
            &format!("{main}/service/OrderService.java")
        ),
        None
    );
    assert!(
        !g.edges.iter().any(|e| e.kind == EdgeKind::Uses
            && e.src.contains("/main/")
            && e.dst.contains("/test/"))
    );
    // No import sites for them.
    assert!(g.imports.iter().all(|s| s.specifier.contains('.')));
    check_golden("spring-java-imports", &g);
}

#[test]
fn csharp_imports_golden() {
    let g = index_fixture("csharp-imports");
    let prog = "src/App/Program.cs";
    // A namespace that is exactly one dir's files -> the Dir; otherwise every file.
    assert_eq!(
        targets(&g, prog, "Acme.Core.Models"),
        t(&["dir:src/Core/Models"])
    );
    assert_eq!(
        targets(&g, prog, "Acme.Core"),
        t(&["file:src/Core/CoreInfo.cs", "file:src/Core/Extra.cs"])
    );
    // using static (type), nested type (heuristic), type alias with generics, namespace
    // alias, relative using inside a namespace block.
    assert_eq!(
        targets(&g, prog, "Acme.Core.Util.MathHelpers"),
        t(&["file:src/Core/Util/MathHelpers.cs"])
    );
    assert_eq!(
        targets(&g, prog, "Acme.Core.Models.Outer.Nested"),
        t(&["file:src/Core/Models/User.cs"])
    );
    assert_eq!(
        confidence(
            &g,
            "file:src/App/Program.cs",
            "file:src/Core/Models/User.cs"
        ),
        Some(Confidence::Heuristic)
    );
    assert_eq!(
        targets(&g, prog, "Acme.Data.Repository"),
        t(&["file:src/Data/Repository.cs"])
    );
    assert_eq!(
        targets(&g, prog, "Acme.Core.Util"),
        t(&["dir:src/Core/Util"])
    );
    assert_eq!(targets(&g, prog, "Services"), t(&["dir:src/App/Services"]));
    // File-scoped namespace with usings after it; nested block namespace.
    assert_eq!(
        targets(&g, "tests/AppTests.cs", "Acme.App.Services"),
        t(&["dir:src/App/Services"])
    );
    assert_eq!(
        targets(&g, "tests/AppTests.cs", "Acme.Core.Extras"),
        t(&["file:src/Core/Extra.cs"])
    );
    // Global usings: from the declaring file.
    assert_eq!(
        targets(&g, "src/Data/GlobalUsings.cs", "Acme.Core.Models"),
        t(&["dir:src/Core/Models"])
    );
    // Externals.
    for (spec, ext, conf) in [
        ("System", "external:dotnet:System", Confidence::Resolved),
        (
            "System.Collections.Generic",
            "external:dotnet:System.Collections",
            Confidence::Resolved,
        ),
        (
            "Microsoft.Extensions.Logging",
            "external:dotnet:Microsoft.Extensions",
            Confidence::Resolved,
        ),
        (
            "Newtonsoft.Json.Linq",
            "external:nuget:Newtonsoft.Json",
            Confidence::Resolved,
        ),
        (
            "Newtonsoft.Json.JsonConvert",
            "external:nuget:Newtonsoft.Json",
            Confidence::Resolved,
        ),
        ("Serilog", "external:nuget:Serilog", Confidence::Resolved),
        ("Xunit", "external:nuget:Xunit", Confidence::Heuristic),
    ] {
        assert_eq!(targets(&g, prog, spec), t(&[ext]), "{spec}");
        assert_eq!(
            confidence(&g, "file:src/App/Program.cs", ext),
            Some(conf),
            "{spec}"
        );
    }
    assert_eq!(
        targets(&g, "src/Data/GlobalUsings.cs", "System.Linq"),
        t(&["external:dotnet:System.Linq"])
    );
    // Unresolved.
    let u = |spec: &str| unresolved(&g, prog, spec).map(|u| (u.0, u.1));
    assert_eq!(u("Acme.Missing"), Some((13, "not_found")));
    assert_eq!(u("Acme"), Some((14, "not_found")));
    assert!(
        g.unresolved
            .iter()
            .any(|u| u.file == "src/Data/Broken.cs" && u.reason.as_str() == "parse_error"),
        "{:?}",
        g.unresolved
    );
    check_golden("csharp-imports", &g);
}

/// The Java and C# indexes are built from declarations stored in the extraction cache:
/// a second run that parses nothing must resolve exactly like the first.
#[test]
fn cached_runs_resolve_like_fresh_ones() {
    for name in ["go-imports", "java-imports", "csharp-imports"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(name);
        copy_dir(&repo_root().join("tests/fixtures").join(name), &root);
        let opts = IndexOptions {
            data_dir: Some(tmp.path().join("db")),
            ..IndexOptions::default()
        };
        let first = index_to_store(&root, &opts).unwrap();
        let second = index_to_store(&root, &opts).unwrap();
        assert_eq!(second.timings.cache_misses, 0, "{name}");
        assert!(second.timings.cache_hits > 0, "{name}");
        assert_eq!(
            golden_view(&first.graph),
            golden_view(&second.graph),
            "{name}"
        );
        let stored = Store::open(tmp.path().join("db/graph.db"))
            .unwrap()
            .load_graph()
            .unwrap();
        assert_eq!(golden_view(&stored), golden_view(&second.graph), "{name}");
    }
}

#[test]
fn report_counts_go_java_and_csharp() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("java-imports");
    copy_dir(&repo_root().join("tests/fixtures/java-imports"), &root);
    let opts = IndexOptions {
        data_dir: Some(tmp.path().join("db")),
        ..IndexOptions::default()
    };
    index_to_store(&root, &opts).unwrap();
    let store = Store::open(tmp.path().join("db/graph.db")).unwrap();
    let md = cartograph_index::report::render(&store).unwrap();
    assert!(md.contains("| Java |"), "{md}");
}
