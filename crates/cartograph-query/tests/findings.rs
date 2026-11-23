//! One fixture repo per kind of finding, checked by hand-written assertions.

mod common;

use cartograph_query::{Config, Finding, FindingKind, Repo, Severity, check, impact, path};
use common::{fixture, index_dir};
use std::fs;
use std::path::Path;

fn ids(r: &Repo, kind: FindingKind) -> Vec<String> {
    r.findings()
        .iter()
        .filter(|f| f.kind == kind)
        .map(|f| f.id.clone())
        .collect()
}

fn get<'a>(r: &'a Repo, id: &str) -> &'a Finding {
    r.findings().iter().find(|f| f.id == id).unwrap_or_else(|| {
        panic!(
            "no finding {id}; have {:?}",
            r.findings().iter().map(|f| &f.id).collect::<Vec<_>>()
        )
    })
}

#[test]
fn query_app_rules_cycles_and_orphans() {
    let r = fixture("query-app");
    // Layer violations: ui → infra is forbidden, core may depend on nothing, infra → core
    // is allowed.
    assert_eq!(
        ids(&r, FindingKind::LayerViolation),
        vec!["layer_violation:core->ui", "layer_violation:ui->infra"]
    );
    let ui = get(&r, "layer_violation:ui->infra");
    assert!(ui.violation && ui.severity == Severity::Error);
    assert_eq!(ui.evidence, vec!["src/ui/app.ts:2 → src/infra/db.ts"]);
    assert_eq!(ui.edges[0].line, Some(2));
    // max_fan_out = 4: big.ts imports five files.
    assert_eq!(
        ids(&r, FindingKind::FanOutLimit),
        vec!["fan_out_limit:file:src/big.ts"]
    );
    // model → repo → service → model, with the shortest loop service → repo → service, is
    // in a no_cycles scope.
    let cyc = get(&r, "cycle:file:src/core/model.ts");
    assert!(cyc.violation, "{cyc:?}");
    assert_eq!(cyc.metrics["files"], 3.0);
    assert_eq!(cyc.metrics["loop"], 2.0);
    assert_eq!(
        cyc.evidence,
        vec![
            "src/core/repo.ts:1 → src/core/service.ts",
            "src/core/service.ts:2 → src/core/repo.ts"
        ]
    );
    // ui ↔ core at folder level (core/util.ts → ui/widgets.ts), cut at the light side.
    let fc = get(&r, "folder_cycle:dir:src/core");
    assert!(
        fc.evidence
            .iter()
            .any(|e| e.starts_with("lightest link to cut: src/core/ → src/ui/ (1)")),
        "{fc:?}"
    );
    // Orphans: dead.ts and big.ts; only-tested.ts is info; main, d.ts, tests, scripts
    // (package.json runs scripts/seed.ts) and config are entries.
    let mut orphans = ids(&r, FindingKind::Orphan);
    orphans.sort();
    assert_eq!(
        orphans,
        vec![
            "orphan:file:src/big.ts",
            "orphan:file:src/dead.ts",
            "orphan:file:src/only-tested.ts"
        ]
    );
    assert_eq!(
        get(&r, "orphan:file:src/only-tested.ts").severity,
        Severity::Info
    );
    // Violations come first.
    assert!(r.findings()[0].violation);
}

#[test]
fn dir_imports_reach_the_files_of_the_package() {
    let r = fixture("findings-go");
    let g = &r.graph;
    let b = g.file_by_path("pkg/b/b.go").unwrap();
    let b2 = g.file_by_path("pkg/b/b2.go").unwrap();
    let a = g.file_by_path("pkg/a/a.go").unwrap();
    // Fan-in: a.go imports the package, so both files of pkg/b (not its test) have it;
    // so does b_test.go, a test of the package itself.
    assert_eq!(g.fan_in(b), 2);
    assert_eq!(g.fan_in(b2), 2);
    assert!(g.out[a as usize].iter().all(|d| d.via_dir));
    let test = g.file_by_path("pkg/b/b_test.go").unwrap();
    assert_eq!(
        g.fan_in(test),
        0,
        "a package import does not reach its tests"
    );
    // Impact: changing b2.go reaches pkg/a and the main package.
    let im = impact::impact(&r, &["pkg/b/b2.go".to_string()])
        .unwrap()
        .data;
    assert_eq!(im.dependents, 2, "{im:?}");
    // Path: main → a → b2.
    let p = path::path(&r, "cmd/app/main.go", "pkg/b/b2.go")
        .unwrap()
        .data;
    let steps: Vec<&str> = p.paths[0].iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        steps,
        vec![
            "file:cmd/app/main.go",
            "file:pkg/a/a.go",
            "file:pkg/b/b2.go"
        ]
    );
    assert_eq!(p.paths[0][0].line, Some(3));
    // Orphans are judged per package: pkg/b is used, pkg/unused is not, pkg/tested only by
    // its own tests (info); a.go and a2.go are never file orphans.
    let orphans: Vec<(String, Severity)> = r
        .findings()
        .iter()
        .filter(|f| f.kind == FindingKind::Orphan)
        .map(|f| (f.id.clone(), f.severity))
        .collect();
    assert_eq!(
        orphans,
        vec![
            ("orphan:dir:pkg/unused".to_string(), Severity::Warning),
            ("orphan:dir:pkg/tested".to_string(), Severity::Info),
        ]
    );
    // Go never reports file-level cycles from package imports.
    assert!(ids(&r, FindingKind::Cycle).is_empty());
    // A test of the package itself needs no import: it is still a test to run.
    let im = impact::impact(&r, &["pkg/tested/t.go".to_string()])
        .unwrap()
        .data;
    let tests: Vec<&str> = im.tests.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(tests, vec!["file:pkg/tested/t_test.go"], "{im:?}");
}

#[test]
fn java_wildcards_and_csharp_namespaces_reach_their_files() {
    // `import com.acme.core.util.*;` reaches Numbers.java and Strings.java.
    let r = fixture("java-imports");
    let g = &r.graph;
    let app = g
        .file_by_path("app/src/main/java/com/acme/app/App.java")
        .unwrap();
    for f in ["Numbers.java", "Strings.java"] {
        let t = g
            .file_by_path(&format!("core/src/main/java/com/acme/core/util/{f}"))
            .unwrap();
        let dep = g.out[app as usize].iter().find(|d| d.file == t);
        assert!(dep.is_some_and(|d| d.via_dir), "App.java → {f}");
        assert!(g.inn[t as usize].iter().any(|d| d.file == app));
    }
    let im = impact::impact(
        &r,
        &["core/src/main/java/com/acme/core/util/Strings.java".to_string()],
    )
    .unwrap()
    .data;
    assert!(
        im.tests
            .iter()
            .any(|t| t.id == "file:core/src/test/java/com/acme/core/ModelTest.java"),
        "{im:?}"
    );

    // `using Acme.Core.Models;` reaches every file of the namespace's folder; the ones
    // only reached that way are `via_dir`, the one also named directly (User.cs) is not.
    let r = fixture("csharp-imports");
    let g = &r.graph;
    let program = g.file_by_path("src/App/Program.cs").unwrap();
    for (f, via) in [
        ("Order.cs", true),
        ("Order.Lines.cs", true),
        ("User.cs", false),
    ] {
        let t = g.file_by_path(&format!("src/Core/Models/{f}")).unwrap();
        let dep = g.out[program as usize].iter().find(|d| d.file == t);
        assert_eq!(dep.map(|d| d.via_dir), Some(via), "Program.cs → {f}");
    }
    let p = path::path(&r, "src/App/Program.cs", "src/Core/Models/Order.Lines.cs")
        .unwrap()
        .data;
    assert_eq!(p.paths[0].len(), 2, "{p:?}");
    assert_eq!(p.paths[0][0].line, Some(7));
}

#[test]
fn dir_import_cycles_are_found() {
    // pkg/x imports pkg/y (dir) and pkg/y imports pkg/x: Go rejects it, but the graph must
    // still see it (a Java wildcard pair is the same shape).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("m");
    let w = |rel: &str, text: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    w("go.mod", "module example.com/m\n");
    w(
        "x/x.go",
        "package x\n\nimport \"example.com/m/y\"\n\nvar A = y.B\n",
    );
    w(
        "y/y.go",
        "package y\n\nimport \"example.com/m/x\"\n\nvar B = x.A\n",
    );
    let r = Repo::new(&index_dir(&root), Config::default());
    let c = ids(&r, FindingKind::Cycle);
    assert_eq!(c, vec!["cycle:file:x/x.go"], "{:?}", r.findings());
    let f = get(&r, "cycle:file:x/x.go");
    assert_eq!(f.metrics["via_dir_edges"], 2.0);
    assert_eq!(
        ids(&r, FindingKind::FolderCycle),
        vec!["folder_cycle:dir:x"]
    );
}

#[test]
fn module_declarations_are_not_cycles() {
    // Rust `mod` + `use crate::…` (and Python `__init__` re-exports) are structure: the only
    // cycle left is two sibling modules that `use` each other.
    let r = fixture("rust-workspace");
    assert_eq!(
        ids(&r, FindingKind::Cycle),
        vec!["cycle:file:crates/core-model/src/graph/edge.rs"],
        "{:?}",
        r.findings()
            .iter()
            .map(|f| (&f.id, &f.evidence))
            .collect::<Vec<_>>()
    );
    // Files declared by `mod` are used; the two nobody declares are not even compiled.
    let mut orphans = ids(&r, FindingKind::Orphan);
    orphans.sort();
    assert_eq!(
        orphans,
        vec![
            "orphan:file:crates/app/src/broken.rs",
            "orphan:file:crates/core-model/src/orphan.rs"
        ]
    );
}

#[test]
fn godot_entry_points_are_not_orphans() {
    let r = fixture("godot-basic");
    let orphans = ids(&r, FindingKind::Orphan);
    for entry in [
        "game/autoload/events.gd",
        "game/autoload/music.tscn",
        "game/scenes/main.tscn",
    ] {
        assert!(
            !orphans.contains(&format!("orphan:file:{entry}")),
            "{entry} is an entry point: {orphans:?}"
        );
    }
}

/// Writes a TS repo whose shape triggers the metric findings, indexes it and returns it.
fn metrics_repo(dir: &Path) -> Repo {
    let w = |rel: &str, text: String| {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    let body = |n: usize| {
        (0..n)
            .map(|i| format!("export const v{i} = {i};\n"))
            .collect::<String>()
    };
    // 30 small leaves.
    for i in 0..30 {
        w(&format!("lib/l{i}.ts"), body(3));
    }
    // A god module: big, imports 12 leaves, imported by 12 features.
    let mut god: String = (0..12)
        .map(|i| format!("import {{ v0 as a{i} }} from \"../lib/l{i}\";\n"))
        .collect();
    god.push_str(&body(400));
    w("core/god.ts", god);
    for j in 0..12 {
        w(
            &format!("feat/f{j}.ts"),
            format!(
                "import {{ v0 }} from \"../core/god\";\nimport {{ v1 }} from \"../lib/l{}\";\n{}",
                12 + j % 6,
                body(4)
            ),
        );
    }
    // A bottleneck: twelve tops reach twelve bases only through mid/hub.ts.
    let mut hub = String::new();
    for k in 0..12 {
        w(&format!("base/b{k}.ts"), body(3));
        hub.push_str(&format!("import {{ v0 as b{k} }} from \"../base/b{k}\";\n"));
        w(
            &format!("top/t{k}.ts"),
            format!("import {{ v0 }} from \"../mid/hub\";\n{}", body(3)),
        );
    }
    hub.push_str(&body(5));
    w("mid/hub.ts", hub);
    // A deep chain c0 → c1 → … → c13.
    for i in 0..14 {
        let next = if i < 13 {
            format!("import {{ v0 }} from \"./c{}\";\n", i + 1)
        } else {
            String::new()
        };
        w(&format!("chain/c{i}.ts"), format!("{next}{}", body(2)));
    }
    // Stable Dependencies: stable/ (used by every feature) leans on volatile/.
    for s in 0..3 {
        w(
            &format!("stable/s{s}.ts"),
            format!("import {{ v0 }} from \"../volatile/u{s}\";\n{}", body(3)),
        );
    }
    for j in 0..12 {
        let p = dir.join(format!("feat/f{j}.ts"));
        let mut t = fs::read_to_string(&p).unwrap();
        t.insert_str(
            0,
            &format!("import {{ v0 as s }} from \"../stable/s{}\";\n", j % 3),
        );
        fs::write(p, t).unwrap();
    }
    for u in 0..3 {
        let imports: String = (20..28)
            .map(|i| format!("import {{ v0 as x{i} }} from \"../lib/l{i}\";\n"))
            .collect();
        w(
            &format!("volatile/u{u}.ts"),
            format!("{imports}{}", body(3)),
        );
    }
    // The entry uses everything else, so nothing is an orphan.
    let entry: String = (0..12)
        .map(|j| format!("import \"./feat/f{j}\";\n"))
        .chain((0..12).map(|k| format!("import \"./top/t{k}\";\n")))
        .chain((0..30).map(|i| format!("import \"./lib/l{i}\";\n")))
        .chain(std::iter::once("import \"./chain/c0\";\n".to_string()))
        .collect();
    w("index.ts", entry);
    Repo::new(&index_dir(dir), Config::default())
}

#[test]
fn metric_findings_on_a_shaped_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let r = metrics_repo(tmp.path());
    let god = r.graph.file_by_path("core/god.ts").unwrap();
    assert_eq!(
        ids(&r, FindingKind::GodModule),
        vec!["god_module:file:core/god.ts"],
        "fan-in {} fan-out {} loc {}; {:?}",
        r.graph.fan_in(god),
        r.graph.fan_out(god),
        r.graph.files[god as usize].loc,
        r.findings()
    );
    assert!(
        ids(&r, FindingKind::Hub).contains(&"hub:file:mid/hub.ts".to_string()),
        "{:?}",
        ids(&r, FindingKind::Hub)
    );
    let chain = ids(&r, FindingKind::DeepChain);
    assert_eq!(chain.len(), 1, "{chain:?}");
    let sdp = ids(&r, FindingKind::Sdp);
    assert!(
        sdp.iter()
            .any(|s| s.starts_with("sdp:dir:stable->dir:volatile")),
        "{sdp:?}"
    );
    // Nothing else is reported on this clean, acyclic repo.
    assert!(ids(&r, FindingKind::Cycle).is_empty());
    assert!(
        ids(&r, FindingKind::Orphan).is_empty(),
        "{:?}",
        ids(&r, FindingKind::Orphan)
    );
}

#[test]
fn thresholds_and_disable_come_from_the_config() {
    let tmp = tempfile::tempdir().unwrap();
    let base = metrics_repo(tmp.path());
    let graph = index_dir(tmp.path());
    let cfg = Config::parse(
        "ignore = [\"chain/**\"]\n[thresholds]\ngod_min_loc = 100000\n[findings]\ndisable = [\"hub\"]\n",
        "t",
    )
    .unwrap();
    let r = Repo::new(&graph, cfg);
    assert!(ids(&r, FindingKind::GodModule).is_empty());
    assert!(ids(&r, FindingKind::Hub).is_empty());
    assert!(ids(&r, FindingKind::DeepChain).is_empty());
    assert!(!ids(&base, FindingKind::DeepChain).is_empty());
}

#[test]
fn findings_are_deterministic() {
    let a = fixture("query-app");
    let b = fixture("query-app");
    let fa: Vec<String> = a
        .findings()
        .iter()
        .map(|f| serde_json::to_string(f).unwrap())
        .collect();
    let fb: Vec<String> = b
        .findings()
        .iter()
        .map(|f| serde_json::to_string(f).unwrap())
        .collect();
    assert_eq!(fa, fb);
}

#[test]
fn evidence_edges_beyond_the_cap_are_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cap");
    let w = |rel: &str, text: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    w("infra/db.ts", "export const db = 1;\n");
    for i in 0..50 {
        w(
            &format!("ui/v{i}.ts"),
            "import { db } from '../infra/db';\nexport const v = db;\n",
        );
    }
    let cfg = Config::parse(
        "[[layers]]\nname = \"ui\"\npaths = [\"ui/**\"]\nforbid = [\"infra\"]\n\n[[layers]]\nname = \"infra\"\npaths = [\"infra/**\"]\n",
        "cartograph.toml",
    )
    .unwrap();
    let r = Repo::new(&index_dir(&root), cfg);
    let f = get(&r, "layer_violation:ui->infra");
    assert_eq!(f.metrics["imports"], 50.0);
    assert_eq!(
        f.edges.len() + f.edges_more,
        50,
        "{} + {}",
        f.edges.len(),
        f.edges_more
    );
    assert!(f.edges_more > 0);
    let json = serde_json::to_value(f).unwrap();
    assert_eq!(json["edges_more"], f.edges_more);
}

/// The file cycle holding `path`.
fn cycle_of<'a>(r: &'a Repo, path: &str) -> &'a Finding {
    let id = format!("file:{path}");
    r.findings()
        .iter()
        .find(|f| f.kind == FindingKind::Cycle && f.nodes.contains(&id))
        .unwrap_or_else(|| panic!("no cycle through {path}: {:?}", r.findings()))
}

/// `tests/fixtures/<name>` copied to a temp dir, `edit` applied, indexed with its config.
fn edited(name: &str, edit: impl FnOnce(&Path)) -> (tempfile::TempDir, Repo) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    common::copy_dir(
        &common::repo_root().join("tests/fixtures").join(name),
        &root,
    );
    edit(&root);
    let g = index_dir(&root);
    let config = Config::load(&root).unwrap();
    (tmp, Repo::new(&g, config))
}

#[test]
fn a_java_cycle_inside_one_package_is_info_never_a_violation() {
    let r = fixture("java-cycles");
    let rules = "src/main/java/com/acme/rules";
    // `no_cycles = true`: the annotation and its validator name each other without an
    // import, inside package com.acme.rules (the unit of design).
    let inside = cycle_of(&r, &format!("{rules}/ValidName.java"));
    assert_eq!(inside.severity, Severity::Info, "{inside:?}");
    assert!(!inside.violation);
    assert!(
        inside
            .title
            .contains("reference each other within package com.acme.rules"),
        "{}",
        inside.title
    );
    // Two packages importing each other: still a violation (a file and a folder cycle).
    let across = cycle_of(&r, "src/main/java/com/acme/left/Left.java");
    assert!(
        across.violation && across.severity == Severity::Error,
        "{across:?}"
    );
    let c = check::check(&r).data;
    assert!(!c.violations.is_empty());
    for v in &c.violations {
        assert!(!v.nodes.iter().any(|n| n.contains("/rules")), "{v:?}");
    }

    // check --base: a new same-package cycle does not fail the check, a new cross-package
    // one does.
    let (_b, base) = edited("java-cycles", |root| {
        let p = root.join("src/main/java/com/acme");
        fs::write(
            p.join("rules/ValidNameValidator.java"),
            "package com.acme.rules;\n\npublic class ValidNameValidator {}\n",
        )
        .unwrap();
        fs::write(
            p.join("right/Right.java"),
            "package com.acme.right;\n\npublic class Right {}\n",
        )
        .unwrap();
    });
    let c = check::check_base(&r, &base, "base").data;
    assert!(!c.violations.is_empty());
    for v in &c.violations {
        assert!(!v.nodes.iter().any(|n| n.contains("/rules")), "{v:?}");
    }
    // A same-package cycle of the base does not stand for a cycle violation of the head:
    // an explicit import of the class turns it into one.
    let (_h, head) = edited("java-cycles", |root| {
        fs::write(
            root.join(format!("{rules}/ValidNameValidator.java")),
            "package com.acme.rules;\n\nimport com.acme.rules.ValidName;\n\
             import jakarta.validation.ConstraintValidator;\n\n\
             public class ValidNameValidator implements ConstraintValidator<ValidName, String> {}\n",
        )
        .unwrap();
    });
    let now = cycle_of(&head, &format!("{rules}/ValidName.java"));
    assert!(now.violation, "{now:?}");
    let c = check::check_base(&head, &r, "base").data;
    assert!(
        c.violations
            .iter()
            .any(|v| v.nodes.iter().any(|n| n.contains("/rules/"))),
        "{:?}",
        c.violations
    );
}
