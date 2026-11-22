//! Entry points: manifest entries, package entries, entries inside test and fixture trees.

mod common;

use cartograph_query::{Config, FindingKind, Repo, overview};
use common::index_dir;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn entries_inside_test_and_fixture_trees_are_not_repo_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("e");
    write(
        &root,
        "package.json",
        r#"{"name":"e","main":"src/main.ts"}"#,
    );
    write(
        &root,
        "src/main.ts",
        "import { u } from './util';\nexport const m = u;\n",
    );
    write(&root, "src/util.ts", "export const u = 1;\n");
    // A fixture package and a fixture Godot project.
    write(
        &root,
        "tests/fixtures/pkg/package.json",
        r#"{"name":"fx","main":"index.js"}"#,
    );
    write(
        &root,
        "tests/fixtures/pkg/index.js",
        "module.exports = 1;\n",
    );
    write(
        &root,
        "tests/fixtures/game/project.godot",
        "[application]\nrun/main_scene=\"res://main.tscn\"\n",
    );
    write(
        &root,
        "tests/fixtures/game/main.tscn",
        "[gd_scene format=3]\n\n[node name=\"Main\" type=\"Node2D\"]\n",
    );
    write(&root, "examples/demo/index.ts", "export const d = 1;\n");
    write(&root, "testdata/app/main.py", "print(1)\n");
    let repo = Repo::new(&index_dir(&root), Config::default());
    let o = overview::overview(&repo, None, 1).unwrap().data;
    let ids: Vec<&str> = o.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["file:src/main.ts"],
        "{}",
        overview::overview(&repo, None, 1).unwrap().text
    );
    // Impact does not list them either.
    let i = cartograph_query::impact::impact(&repo, &["src/util.ts".to_string()])
        .unwrap()
        .data;
    assert_eq!(i.entries, vec!["file:src/main.ts"]);
}

/// A Rust binary whose modules `use crate::…` from its `main.rs` still starts there:
/// `overview` lists it and `impact` of a library module reaches it, through a library path
/// written without a `use` (`app_lib::run()`).
#[test]
fn a_crate_root_its_modules_use_is_still_an_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("w");
    write(
        &root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"lib\"]\n",
    );
    write(
        &root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nedition = \"2021\"\n[dependencies]\n\
         app-lib = { path = \"../lib\" }\n",
    );
    write(
        &root,
        "app/src/main.rs",
        "mod cmd;\npub struct Opts;\nfn main() {\n    cmd::go(Opts);\n    app_lib::run();\n}\n",
    );
    write(
        &root,
        "app/src/cmd.rs",
        "use crate::Opts;\npub fn go(_o: Opts) {}\n",
    );
    write(
        &root,
        "lib/Cargo.toml",
        "[package]\nname = \"app-lib\"\nedition = \"2021\"\n",
    );
    write(
        &root,
        "lib/src/lib.rs",
        "mod engine;\npub fn run() { engine::step(); }\n",
    );
    write(&root, "lib/src/engine.rs", "pub fn step() {}\n");
    let repo = Repo::new(&index_dir(&root), Config::default());
    let o = overview::overview(&repo, None, 1).unwrap().data;
    let ids: Vec<&str> = o.entries.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"file:app/src/main.rs"), "{ids:?}");
    let i = cartograph_query::impact::impact(&repo, &["lib/src/engine.rs".to_string()])
        .unwrap()
        .data;
    assert_eq!(i.entries, vec!["file:app/src/main.rs"]);
}

/// A crate's `lib.rs` that its own `main.rs` and `src/bin/*.rs` import is a library they
/// use, not an entry: those binaries are sibling crate roots, not its submodules.
#[test]
fn a_lib_its_binaries_use_is_not_an_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("c");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = \"app\"\nedition = \"2021\"\n",
    );
    write(
        &root,
        "src/lib.rs",
        "pub mod engine;\npub fn run() { engine::step(); }\n",
    );
    write(
        &root,
        "src/engine.rs",
        "use crate::run as _r;\npub fn step() {}\n",
    );
    write(
        &root,
        "src/main.rs",
        "use app::run;\nfn main() {\n    run();\n}\n",
    );
    write(
        &root,
        "src/bin/tool.rs",
        "use app::engine;\nfn main() {\n    engine::step();\n}\n",
    );
    let repo = Repo::new(&index_dir(&root), Config::default());
    let o = overview::overview(&repo, None, 1).unwrap();
    let ids: Vec<&str> = o.data.entries.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"file:src/main.rs"), "{}", o.text);
    assert!(ids.contains(&"file:src/bin/tool.rs"), "{}", o.text);
    assert!(!ids.contains(&"file:src/lib.rs"), "{}", o.text);
}

#[test]
fn an_overview_inside_an_examples_tree_lists_its_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("x");
    write(
        &root,
        "package.json",
        r#"{"name":"x","main":"src/main.ts"}"#,
    );
    write(&root, "src/main.ts", "export const m = 1;\n");
    // An example with a manifest, and a fixture inside it.
    write(
        &root,
        "examples/web-app/package.json",
        r#"{"name":"web","main":"server.js"}"#,
    );
    write(&root, "examples/web-app/server.js", "module.exports = 1;\n");
    write(
        &root,
        "examples/web-app/tests/fixtures/pkg/package.json",
        r#"{"name":"fx","main":"index.js"}"#,
    );
    write(
        &root,
        "examples/web-app/tests/fixtures/pkg/index.js",
        "module.exports = 1;\n",
    );
    // An example without one: its name says it is the entry.
    write(
        &root,
        "examples/cli-demo/src/main.ts",
        "import { u } from './util';\nexport const m = u;\n",
    );
    write(
        &root,
        "examples/cli-demo/src/util.ts",
        "export const u = 1;\n",
    );
    let repo = Repo::new(&index_dir(&root), Config::default());
    let entries = |scope: Option<&str>| -> Vec<String> {
        overview::overview(&repo, scope, 2)
            .unwrap()
            .data
            .entries
            .iter()
            .map(|e| e.id.clone())
            .collect()
    };
    // From the repo root, examples are not where the repo starts.
    assert_eq!(entries(None), vec!["file:src/main.ts"]);
    // Inside one, they are; a fixture below the scope still is not.
    assert_eq!(
        entries(Some("examples/web-app")),
        vec!["file:examples/web-app/server.js"]
    );
    assert_eq!(
        entries(Some("examples/cli-demo")),
        vec!["file:examples/cli-demo/src/main.ts"]
    );
    // Impact's scope is the repo: an example is not an entry point it reaches.
    let i = cartograph_query::impact::impact(&repo, &["examples/cli-demo/src/util.ts".into()])
        .unwrap()
        .data;
    assert!(i.entries.is_empty(), "{:?}", i.entries);
}

#[test]
fn manifest_entries_without_extension_or_pointing_at_a_folder_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("m");
    write(
        &root,
        "package.json",
        r#"{"name":"m","main":"lib/server","bin":{"m":"./cli"},"exports":{".":"./api"}}"#,
    );
    write(&root, "lib/server.js", "module.exports = 1;\n");
    write(&root, "cli/index.ts", "export {};\n");
    write(&root, "api.mjs", "export const a = 1;\n");
    let repo = Repo::new(&index_dir(&root), Config::default());
    let g = &repo.graph;
    for p in ["lib/server.js", "cli/index.ts", "api.mjs"] {
        let f = g.file_by_path(p).unwrap();
        assert_eq!(g.files[f as usize].entry, Some("manifest entry"), "{p}");
    }
    assert!(
        !repo
            .findings()
            .iter()
            .any(|f| f.kind == FindingKind::Orphan),
        "{:?}",
        repo.findings()
    );
}

#[test]
fn manifest_entries_with_dot_segments_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("d");
    // A root package whose main is the package folder itself.
    write(&root, "package.json", r#"{"name":"d","main":"."}"#);
    write(&root, "index.ts", "export const r = 1;\n");
    write(
        &root,
        "pkg/package.json",
        r#"{"name":"p","main":".","bin":{"p":"./lib/./cli"},"exports":{"./api":"./src/./api.mjs"}}"#,
    );
    write(&root, "pkg/index.js", "module.exports = 1;\n");
    write(&root, "pkg/lib/cli.ts", "export {};\n");
    write(&root, "pkg/src/api.mjs", "export const a = 1;\n");
    let repo = Repo::new(&index_dir(&root), Config::default());
    let g = &repo.graph;
    for p in [
        "index.ts",
        "pkg/index.js",
        "pkg/lib/cli.ts",
        "pkg/src/api.mjs",
    ] {
        let f = g.file_by_path(p).unwrap();
        assert_eq!(g.files[f as usize].entry, Some("manifest entry"), "{p}");
    }
}

#[test]
fn a_java_package_named_example_keeps_its_entry_point() {
    // Spring Initializr's default package: `com/example/` is a package, not samples.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("j");
    write(
        &root,
        "src/main/java/com/example/demo/DemoApplication.java",
        "package com.example.demo;\n\npublic class DemoApplication {\n    public static void main(String[] args) {}\n}\n",
    );
    let repo = Repo::new(&index_dir(&root), Config::default());
    let o = overview::overview(&repo, None, 1).unwrap().data;
    let ids: Vec<&str> = o.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["file:src/main/java/com/example/demo/DemoApplication.java"]
    );
}

#[test]
fn package_entry_is_reachable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("p");
    write(&root, "pkg/package.json", r#"{"name":"p"}"#);
    write(&root, "pkg/index.ts", "export const a = 1;\n");
    write(&root, "pkg/other.ts", "export const b = 1;\n");
    let repo = Repo::new(&index_dir(&root), Config::default());
    let g = &repo.graph;
    let entry = |p: &str| g.files[g.file_by_path(p).unwrap() as usize].entry;
    assert_eq!(entry("pkg/index.ts"), Some("package entry"));
    assert_eq!(entry("pkg/other.ts"), Some("top-level script"));
}

#[test]
fn only_real_entry_names_are_package_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("p");
    write(&root, "pkg/package.json", r#"{"name":"p"}"#);
    for name in ["main", "index", "app", "server", "cli", "lib", "mod"] {
        write(&root, &format!("pkg/{name}.ts"), "export const a = 1;\n");
    }
    // Entry names of other kinds: a test config, a hook, a service worker, an init file.
    for name in [
        "setup.ts",
        "conftest.py",
        "__init__.py",
        "middleware.ts",
        "instrumentation.ts",
        "sw.js",
    ] {
        write(&root, &format!("pkg/{name}"), "x = 1\n");
    }
    let repo = Repo::new(&index_dir(&root), Config::default());
    let g = &repo.graph;
    let entry = |p: &str| g.files[g.file_by_path(p).unwrap() as usize].entry;
    for name in ["main", "index", "app", "server", "cli", "lib", "mod"] {
        let p = format!("pkg/{name}.ts");
        assert_eq!(entry(&p), Some("package entry"), "{p}");
    }
    for p in [
        "pkg/setup.ts",
        "pkg/conftest.py",
        "pkg/__init__.py",
        "pkg/middleware.ts",
        "pkg/instrumentation.ts",
        "pkg/sw.js",
    ] {
        assert_ne!(entry(p), Some("package entry"), "{p}");
    }
    assert_eq!(entry("pkg/middleware.ts"), Some("top-level script"));
}
