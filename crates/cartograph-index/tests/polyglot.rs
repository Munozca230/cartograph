//! Integration tests against `tests/fixtures/polyglot`, indexed from a temp copy so the
//! source tree stays clean and the repo's own `.gitignore` cannot leak in.

use cartograph_core::{EdgeKind, Graph, NodeKind, SkipReason, Store, node_id};
use cartograph_index::{IndexOptions, db_path, index, index_to_store};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/polyglot")
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

fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// Temp copy of the fixture plus the paths that cannot be committed: always-excluded dirs
/// and a file over the 1 MB limit.
fn prepared_copy() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("polyglot");
    copy_dir(&fixture(), &root);
    write(&root, ".git/HEAD", b"ref: refs/heads/main\n");
    write(
        &root,
        "node_modules/left-pad/index.js",
        b"module.exports = 1;\n",
    );
    write(
        &root,
        "web/node_modules/x/index.js",
        b"module.exports = 2;\n",
    );
    write(&root, "target/debug/out.rs", b"fn main() {}\n");
    write(&root, "big/huge.txt", &vec![b'a'; 1024 * 1024 + 1]);
    tmp
}

fn root_of(tmp: &tempfile::TempDir) -> PathBuf {
    tmp.path().join("polyglot")
}

fn files_by_path(g: &Graph) -> BTreeMap<&str, &cartograph_core::Node> {
    g.nodes_of(NodeKind::File)
        .map(|n| (n.path.as_str(), n))
        .collect()
}

#[test]
fn fixture_files_are_committed_as_expected() {
    // Sanity checks on the fixture itself (fail loudly if a checkout mangled it).
    let crlf = fs::read(fixture().join("crlf/windows.py")).unwrap();
    assert!(
        crlf.windows(2).any(|w| w == b"\r\n"),
        "crlf/windows.py lost its CRLF line endings"
    );
    assert!(fixture().join("web/generated/bundle.js").is_file());
    assert!(fixture().join("web/debug.log").is_file());
    assert!(fixture().join("src/módulo extra/útil.py").is_file());
}

#[test]
fn indexes_the_polyglot_fixture() {
    let tmp = prepared_copy();
    let out = index(&root_of(&tmp), &IndexOptions::default()).unwrap();
    let g = &out.graph;
    let files = files_by_path(g);

    let expected: BTreeMap<&str, Option<&str>> = [
        ("Cargo.toml", None),
        ("README.md", None),
        ("c/util.c", Some("c")),
        ("c/util.h", Some("c")),
        ("cpp/engine.cpp", Some("cpp")),
        ("cpp/engine.hpp", Some("cpp")),
        ("crlf/windows.py", Some("python")),
        ("cs/Greeter.cs", Some("csharp")),
        ("cs/Program.cs", Some("csharp")),
        ("game/autoload/game_state.gd", Some("gdscript")),
        ("game/main.tscn", Some("godot-scene")),
        ("game/player.gd", Some("gdscript")),
        ("game/project.godot", Some("godot-project")),
        ("game/theme.tres", Some("godot-resource")),
        ("go/handler.go", Some("go")),
        ("go/server.go", Some("go")),
        ("java/Util.java", Some("java")),
        ("java/App.java", Some("java")),
        ("package.json", None),
        ("py/app.py", Some("python")),
        ("py/models.py", Some("python")),
        ("src/lib.rs", Some("rust")),
        ("src/main.rs", Some("rust")),
        ("src/módulo extra/útil.py", Some("python")),
        ("web/.gitignore", None),
        ("web/Button.tsx", Some("tsx")),
        ("web/config.mjs", Some("javascript")),
        ("web/app.ts", Some("typescript")),
        ("web/component.tsx", Some("tsx")),
        ("web/types.ts", Some("typescript")),
        ("web/util.js", Some("javascript")),
    ]
    .into_iter()
    .collect();
    let got: BTreeMap<&str, Option<&str>> =
        files.iter().map(|(p, n)| (*p, n.lang.as_deref())).collect();
    assert_eq!(got, expected);

    // Nested .gitignore (web/.gitignore) is honoured even without a .git directory.
    assert!(!files.contains_key("web/generated/bundle.js"));
    assert!(!files.contains_key("web/debug.log"));

    // Skipped: every one recorded with its reason.
    let skipped: BTreeMap<&str, SkipReason> = g
        .skipped
        .iter()
        .map(|s| (s.path.as_str(), s.reason))
        .collect();
    let expected_skipped: BTreeMap<&str, SkipReason> = [
        (".git", SkipReason::ExcludedDir),
        ("assets/logo.png", SkipReason::Binary),
        ("big/huge.txt", SkipReason::TooLarge),
        ("node_modules", SkipReason::ExcludedDir),
        ("target", SkipReason::ExcludedDir),
        ("web/node_modules", SkipReason::ExcludedDir),
    ]
    .into_iter()
    .collect();
    assert_eq!(skipped, expected_skipped);

    // Paths: repo-relative, forward slashes, no leading "./" or "/".
    for n in &g.nodes {
        assert!(!n.path.contains('\\'), "backslash in {:?}", n.path);
        assert!(
            !n.path.starts_with('/') && !n.path.starts_with("./"),
            "{:?}",
            n.path
        );
    }
    for s in &g.skipped {
        assert!(!s.path.contains('\\'), "backslash in {:?}", s.path);
    }

    // LOC = non-blank lines; CRLF blank lines (lone `\r`, spaces + `\r`) are blank.
    assert_eq!(files["crlf/windows.py"].loc, 3);
    assert_eq!(files["src/main.rs"].loc, 5);
    let readme = fs::read(root_of(&tmp).join("README.md")).unwrap();
    assert_eq!(files["README.md"].bytes, readme.len() as u64);
    assert_eq!(
        files["README.md"].hash.as_deref(),
        Some(blake3::hash(&readme).to_hex().as_str())
    );

    // Dirs (web/generated is ignored, so it has no node).
    let dirs: Vec<&str> = g.nodes_of(NodeKind::Dir).map(|n| n.path.as_str()).collect();
    assert_eq!(
        dirs,
        vec![
            "assets",
            "big",
            "c",
            "cpp",
            "crlf",
            "cs",
            "game",
            "game/autoload",
            "go",
            "java",
            "py",
            "src",
            "src/módulo extra",
            "web"
        ]
    );
    let game = g.node(&node_id(NodeKind::Dir, "game")).unwrap();
    assert_eq!(game.meta["files"], 5);

    // Packages from manifests.
    let mut packages: Vec<(&str, &str, &str)> = g
        .nodes_of(NodeKind::Package)
        .map(|n| {
            (
                n.path.as_str(),
                n.name.as_str(),
                n.meta["manifest"].as_str().unwrap(),
            )
        })
        .collect();
    packages.sort();
    assert_eq!(
        packages,
        vec![
            ("Cargo.toml", "polyglot-rs", "cargo"),
            ("game/project.godot", "Polyglot Game", "godot"),
            ("package.json", "polyglot-web", "npm"),
        ]
    );
    let godot = g
        .node(&node_id(NodeKind::Package, "game/project.godot"))
        .unwrap();
    assert_eq!(godot.meta["details"]["autoloads"][0]["name"], "GameState");
    assert_eq!(
        godot.meta["details"]["autoloads"][0]["path"],
        "res://autoload/game_state.gd"
    );
    let npm = g.node(&node_id(NodeKind::Package, "package.json")).unwrap();
    assert_eq!(npm.meta["details"]["workspaces"][0], "web");
    assert_eq!(out.manifests.len(), 3);

    // Containment: every Dir/File has exactly one Repo/Dir container and every Package
    // one Repo container; a Package also contains the Dir of its manifest (root packages
    // have none); externals have no container. Every edge endpoint exists.
    let ids: std::collections::HashSet<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut parents: BTreeMap<&str, usize> = BTreeMap::new();
    let mut package_dirs: Vec<(&str, &str)> = Vec::new();
    for e in &g.edges {
        assert!(ids.contains(e.src.as_str()), "dangling src {}", e.src);
        assert!(ids.contains(e.dst.as_str()), "dangling dst {}", e.dst);
        if e.kind == EdgeKind::Contains {
            if e.src.starts_with("package:") {
                package_dirs.push((e.src.as_str(), e.dst.as_str()));
            } else {
                *parents.entry(e.dst.as_str()).or_default() += 1;
            }
        }
    }
    for n in &g.nodes {
        let want = usize::from(!matches!(n.kind, NodeKind::Repo | NodeKind::External));
        assert_eq!(
            parents.get(n.id.as_str()).copied().unwrap_or(0),
            want,
            "{}",
            n.id
        );
    }
    assert_eq!(
        package_dirs,
        vec![("package:game/project.godot", "dir:game")]
    );

    // N1 imports in the polyglot fixture.
    let import = |src: &str, dst: &str| {
        g.edges
            .iter()
            .find(|e| e.src == src && e.dst == dst)
            .map(|e| (e.kind, e.confidence))
    };
    use cartograph_core::Confidence;
    assert_eq!(
        import("file:web/app.ts", "file:web/util.js"),
        Some((EdgeKind::Import, Confidence::Exact))
    );
    // py/ is not a package or a source root: script-dir resolution is a guess.
    assert_eq!(
        import("file:py/app.py", "file:py/models.py"),
        Some((EdgeKind::Import, Confidence::Heuristic))
    );
    assert_eq!(
        import("file:py/models.py", "external:python:dataclasses"),
        Some((EdgeKind::ImportExternal, Confidence::Exact))
    );
    assert!(
        g.edges
            .iter()
            .any(|e| e.src == "dir:src/módulo extra" && e.dst == "file:src/módulo extra/útil.py")
    );
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::Manifest
        && e.src == "package:Cargo.toml"
        && e.dst == "file:Cargo.toml"));

    // Repo totals are the sum over files.
    let repo = g.node("repo:").unwrap();
    assert_eq!(repo.name, "polyglot");
    assert_eq!(repo.loc, files.values().map(|f| f.loc).sum::<u64>());
    assert_eq!(repo.meta["files"], 31);
}

#[test]
fn store_round_trips_and_reindex_is_idempotent() {
    let tmp = prepared_copy();
    let root = root_of(&tmp);
    let first = index_to_store(&root, &IndexOptions::default()).unwrap();
    let store = Store::open(db_path(&root)).unwrap();
    assert_eq!(store.load_graph().unwrap(), first.graph);
    assert_eq!(store.journal_mode().unwrap(), "wal");
    assert!(store.get_meta("timings").unwrap().is_some());
    assert!(store.get_meta("manifests").unwrap().is_some());

    // .cartograph now exists inside the repo; it must be excluded, not indexed.
    let second = index_to_store(&root, &IndexOptions::default()).unwrap();
    let mut expected = first.graph.clone();
    expected.skipped.push(cartograph_core::SkippedFile {
        path: ".cartograph".into(),
        reason: SkipReason::ExcludedDir,
        detail: None,
    });
    expected.normalize();
    assert_eq!(second.graph, expected);
    assert_eq!(store.load_graph().unwrap(), expected);
}

#[test]
fn malformed_manifest_is_recorded_and_indexing_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "package.json", b"{ not json");
    write(root, "lib/Cargo.toml", b"[package\nname = 'x'");
    write(root, "lib/ok.rs", b"fn ok() {}\n");
    let out = index(root, &IndexOptions::default()).unwrap();
    let reasons: Vec<(&str, SkipReason)> = out
        .graph
        .skipped
        .iter()
        .map(|s| (s.path.as_str(), s.reason))
        .collect();
    assert_eq!(
        reasons,
        vec![
            ("lib/Cargo.toml", SkipReason::ParseError),
            ("package.json", SkipReason::ParseError)
        ]
    );
    // The files are still indexed, and packages exist with a fallback name.
    assert_eq!(out.graph.nodes_of(NodeKind::File).count(), 3);
    let lib = out
        .graph
        .node(&node_id(NodeKind::Package, "lib/Cargo.toml"))
        .unwrap();
    assert_eq!(lib.name, "lib");
    assert!(lib.meta["error"].is_string());
}

/// A file another process holds open without sharing cannot be read; it must be recorded
/// as `read_error` and the rest of the repo still indexed.
#[cfg(windows)]
#[test]
fn unreadable_file_is_recorded_and_indexing_continues() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "locked.py",
        b"x = 1
",
    );
    write(
        root, "ok.py", b"y = 2
",
    );
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(root.join("locked.py"))
        .unwrap();
    let out = index(root, &IndexOptions::default()).unwrap();
    assert_eq!(out.graph.skipped.len(), 1, "{:?}", out.graph.skipped);
    assert_eq!(out.graph.skipped[0].path, "locked.py");
    assert_eq!(out.graph.skipped[0].reason, SkipReason::ReadError);
    assert!(out.graph.node("file:ok.py").is_some());
}

/// Names that are not valid Unicode get an escaped, repo-relative path; two such names
/// must not collide (the store's key would reject the whole index).
#[cfg(windows)]
#[test]
fn invalid_unicode_names_are_recorded_with_distinct_relative_paths() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "sub/ok.py", b"x = 1\n");
    for unit in [0xD800u16, 0xD801] {
        let name = OsString::from_wide(&[unit, u16::from(b'a')]);
        fs::write(root.join("sub").join(name), b"x").unwrap();
    }
    let out = index_to_store(root, &IndexOptions::default()).unwrap();
    let invalid: Vec<&str> = out
        .graph
        .skipped
        .iter()
        .filter(|s| s.reason == SkipReason::InvalidPath)
        .map(|s| s.path.as_str())
        .collect();
    assert_eq!(invalid, vec!["sub/%uD800a", "sub/%uD801a"]);
    let stored = Store::open(db_path(root)).unwrap().load_graph().unwrap();
    assert_eq!(stored, out.graph);
}

#[cfg(unix)]
#[test]
fn invalid_unicode_names_are_recorded_with_distinct_relative_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "sub/ok.py", b"x = 1\n");
    for byte in [0xFFu8, 0xFE] {
        let name = OsStr::from_bytes(&[byte, b'a']);
        fs::write(root.join("sub").join(name), b"x").unwrap();
    }
    let out = index_to_store(root, &IndexOptions::default()).unwrap();
    let invalid: Vec<&str> = out
        .graph
        .skipped
        .iter()
        .filter(|s| s.reason == SkipReason::InvalidPath)
        .map(|s| s.path.as_str())
        .collect();
    assert_eq!(invalid, vec!["sub/%FEa", "sub/%FFa"]);
}

/// A malformed `.gitignore` line is reported, and the valid lines still apply.
#[test]
fn gitignore_parse_errors_are_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "sub/.gitignore", b"[z-a]\n*.log\n");
    write(root, "sub/a.log", b"log\n");
    write(root, "sub/b.py", b"x = 1\n");
    let out = index(root, &IndexOptions::default()).unwrap();
    assert!(out.graph.node("file:sub/b.py").is_some());
    assert!(out.graph.node("file:sub/a.log").is_none());
    let s: Vec<(&str, SkipReason)> = out
        .graph
        .skipped
        .iter()
        .map(|s| (s.path.as_str(), s.reason))
        .collect();
    assert_eq!(s, vec![("sub/.gitignore", SkipReason::ParseError)]);
    assert!(
        out.graph.skipped[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("z-a")
    );
}

#[test]
fn index_meta_is_written_with_the_graph() {
    let tmp = prepared_copy();
    let root = root_of(&tmp);
    let out = index_to_store(&root, &IndexOptions::default()).unwrap();
    let (graph, meta) = Store::open(db_path(&root))
        .unwrap()
        .load_snapshot()
        .unwrap();
    assert_eq!(graph, out.graph);
    for key in [
        "repo_name",
        "manifests",
        "timings",
        "indexer_version",
        "indexed_at_unix",
    ] {
        assert!(meta.contains_key(key), "{key} missing: {meta:?}");
    }
}

#[test]
fn rejects_a_missing_root() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(index(&tmp.path().join("nope"), &IndexOptions::default()).is_err());
}

#[test]
fn a_data_dir_inside_the_repo_is_excluded_from_the_walk() {
    let tmp = prepared_copy();
    let root = root_of(&tmp);
    let opts = IndexOptions {
        data_dir: Some(root.join("out").join("db")),
        ..IndexOptions::default()
    };
    // Twice: the second run sees the database written by the first.
    index_to_store(&root, &opts).unwrap();
    let out = index_to_store(&root, &opts).unwrap();
    assert!(root.join("out/db/graph.db").is_file());
    assert!(!root.join(".cartograph").exists());
    let g = &out.graph;
    assert!(
        !g.nodes.iter().any(|n| n.path.starts_with("out/db")),
        "data dir was indexed"
    );
    assert!(
        g.skipped
            .iter()
            .any(|s| s.path == "out/db" && s.reason == SkipReason::ExcludedDir)
    );
}

#[test]
fn the_legacy_codevgraph_data_dir_is_still_excluded() {
    let tmp = prepared_copy();
    let root = root_of(&tmp);
    write(&root, ".codevgraph/graph.db", b"SQLite format 3\0");
    write(&root, ".codevgraph/notes.txt", b"old index\n");
    let g = index(&root, &IndexOptions::default()).unwrap().graph;
    assert!(!g.nodes.iter().any(|n| n.path.starts_with(".codevgraph")));
    assert!(
        g.skipped
            .iter()
            .any(|s| s.path == ".codevgraph" && s.reason == SkipReason::ExcludedDir)
    );
}
