//! TypeScript type-only dependencies (`import type`, `export type … from`, `import("…")` in
//! a type): not runtime dependencies for findings (cycles, folder cycles, Stable
//! Dependencies, hubs, god modules), still dependencies for `deps`, `impact` and `path`,
//! where they are marked. A list whose every specifier is `type` (`import { type A }`,
//! `export { type A } from`) is a runtime dependency: it can still load the module.
//! Fixture: `tests/fixtures/ts-type-only`.

mod common;

use cartograph_core::Store;
use cartograph_query::deps::{Direction, Level};
use cartograph_query::{Config, FindingKind, Repo, deps, impact, path};
use common::{copy_dir, fixture, index_dir, repo_root};
use std::fs;

fn dep(r: &Repo, a: &str, b: &str) -> cartograph_query::graph::Dep {
    let g = &r.graph;
    let (fa, fb) = (g.file_by_path(a).unwrap(), g.file_by_path(b).unwrap());
    *g.out[fa as usize]
        .iter()
        .find(|d| d.file == fb)
        .unwrap_or_else(|| panic!("no dependency {a} → {b}"))
}

fn kinds(r: &Repo, k: FindingKind) -> Vec<String> {
    r.findings()
        .iter()
        .filter(|f| f.kind == k)
        .map(|f| f.id.clone())
        .collect()
}

#[test]
fn type_only_edges_are_marked_in_the_graph() {
    let r = fixture("ts-type-only");
    assert!(dep(&r, "src/model/user.ts", "src/auth/session.ts").type_only);
    assert!(dep(&r, "src/auth/token.ts", "src/model/user.ts").type_only);
    assert!(dep(&r, "src/auth/token.ts", "src/util/hash.ts").type_only);
    assert!(dep(&r, "src/util/hash.ts", "src/auth/token.ts").type_only);
    // `import { type A, type B }` and `export { type A } from` without `import type` /
    // `export type`: `import {} from …` under `verbatimModuleSyntax`, the module loads.
    assert!(!dep(&r, "src/auth/token.ts", "src/auth/session.ts").type_only);
    assert!(!dep(&r, "src/main.ts", "src/util/hash.ts").type_only);
    // A value next to a type, and a dynamic import(), load the module.
    assert!(!dep(&r, "src/main.ts", "src/auth/session.ts").type_only);
    assert!(!dep(&r, "src/main.ts", "src/model/user.ts").type_only);
    assert!(!dep(&r, "src/auth/session.ts", "src/model/user.ts").type_only);
    assert!(!dep(&r, "src/model/user.ts", "src/util/hash.ts").type_only);
}

#[test]
fn type_only_edges_survive_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ts-type-only");
    copy_dir(&repo_root().join("tests/fixtures/ts-type-only"), &root);
    let mut store = Store::open(tmp.path().join("graph.db")).unwrap();
    store.replace_graph(&index_dir(&root)).unwrap();
    let r = Repo::new(&store.load_graph().unwrap(), Config::default());
    assert!(dep(&r, "src/model/user.ts", "src/auth/session.ts").type_only);
    assert!(!dep(&r, "src/auth/session.ts", "src/model/user.ts").type_only);
}

#[test]
fn type_only_edges_make_no_cycles() {
    let r = fixture("ts-type-only");
    assert!(
        kinds(&r, FindingKind::Cycle).is_empty(),
        "{:?}",
        r.findings()
    );
    assert!(
        kinds(&r, FindingKind::FolderCycle).is_empty(),
        "{:?}",
        r.findings()
    );
    // The same repo with value imports has both: the test above can fail.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ts-runtime");
    copy_dir(&repo_root().join("tests/fixtures/ts-type-only"), &root);
    let user = root.join("src/model/user.ts");
    let text = fs::read_to_string(&user).unwrap();
    fs::write(&user, text.replace("import type ", "import ")).unwrap();
    let runtime = Repo::new(&index_dir(&root), Config::default());
    assert!(!kinds(&runtime, FindingKind::Cycle).is_empty());
    assert!(!kinds(&runtime, FindingKind::FolderCycle).is_empty());
}

#[test]
fn deps_impact_and_path_keep_type_only_edges_and_mark_them() {
    let r = fixture("ts-type-only");
    // deps
    let d = deps::deps(&r, "src/model/user.ts", Direction::Out, 1, Level::File).unwrap();
    let uses = d.data.uses.as_ref().unwrap();
    let session = uses
        .iter()
        .find(|b| b.id == "file:src/auth/session.ts")
        .expect("a type-only dependency is still listed");
    assert!(session.type_only);
    assert!(
        !uses
            .iter()
            .find(|b| b.id == "file:src/util/hash.ts")
            .unwrap()
            .type_only
    );
    assert!(
        d.text.contains("src/auth/session.ts :2 (type)"),
        "{}",
        d.text
    );
    // impact: user.ts and hash.ts only reach session.ts through types (hash.ts through
    // token.ts, whose all-`type` import of session.ts is a runtime one).
    let i = impact::impact(&r, &["src/auth/session.ts".to_string()]).unwrap();
    assert!(i.data.dependents >= 3, "{}", i.text);
    assert!(i.data.type_only >= 2, "{}", i.text);
    assert!(i.text.contains("through types only"), "{}", i.text);
    let t = i
        .data
        .tests
        .iter()
        .find(|t| t.id == "file:src/main.test.ts")
        .expect("a test reaching it through a type import still runs");
    assert!(t.type_only);
    // path
    let p = path::path(&r, "src/model/user.ts", "src/auth/session.ts").unwrap();
    assert!(p.data.paths[0][0].type_only, "{:?}", p.data.paths);
    assert!(p.text.contains("(type)"), "{}", p.text);
    let json = serde_json::to_value(&p.data).unwrap();
    assert_eq!(json["paths"][0][0]["type_only"], true);
}

#[test]
fn a_file_used_only_for_types_is_not_an_orphan() {
    let r = fixture("ts-type-only");
    let orphans = kinds(&r, FindingKind::Orphan);
    assert!(
        !orphans.contains(&"orphan:file:src/auth/token.ts".to_string()),
        "{orphans:?}"
    );
}
