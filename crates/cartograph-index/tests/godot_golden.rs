//! Golden test for Godot: GDScript references and the scene / resource / project parser.
//!
//! `tests/fixtures/godot-basic` is indexed from a temp copy and compared exactly with
//! `tests/golden/godot-basic.json`: import, extends, call and derived edges, import sites,
//! unresolved records, externals, diagnostics and the Godot metadata of file nodes.
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test godot_golden`
//! and review the diff. The hand-written assertions below pin the cases the golden file
//! must keep.

use cartograph_core::{Confidence, EdgeKind, Graph, NodeKind, Store};
use cartograph_index::{IndexOptions, db_path, index, index_to_store};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURE: &str = "godot-basic";

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

fn copy_fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(FIXTURE);
    copy_dir(&repo_root().join("tests/fixtures").join(FIXTURE), &root);
    (tmp, root)
}

fn index_fixture() -> Graph {
    let (_tmp, root) = copy_fixture();
    index(&root, &IndexOptions::default()).unwrap().graph
}

fn golden_view(g: &Graph) -> Value {
    let edges: Vec<Value> = g
        .edges
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EdgeKind::Import
                    | EdgeKind::ImportExternal
                    | EdgeKind::DependsOn
                    | EdgeKind::Extends
                    | EdgeKind::Call
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
    let meta: serde_json::Map<String, Value> = g
        .nodes_of(NodeKind::File)
        .filter(|n| !n.meta.is_null())
        .map(|n| (n.path.clone(), n.meta.clone()))
        .collect();
    json!({
        "edges": edges,
        "sites": sites,
        "unresolved": unresolved,
        "externals": externals,
        "diagnostics": diagnostics,
        "file_meta": meta,
    })
}

#[test]
fn godot_basic_golden() {
    let g = index_fixture();
    let path = repo_root()
        .join("tests/golden")
        .join(format!("{FIXTURE}.json"));
    let got = golden_view(&g);
    let text = format!("{}\n", serde_json::to_string_pretty(&got).unwrap());
    if std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &text).unwrap();
    } else {
        let want: Value = serde_json::from_str(
            &fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display())),
        )
        .unwrap();
        if want != got {
            let diff = std::env::temp_dir().join(format!("cartograph-{FIXTURE}.actual.json"));
            fs::write(&diff, &text).unwrap();
            panic!(
                "{FIXTURE}: graph differs from {} (actual written to {})",
                path.display(),
                diff.display()
            );
        }
    }
    pinned(&g);
}

fn edge(g: &Graph, src: &str, dst: &str, kind: EdgeKind) -> Option<Confidence> {
    g.edges
        .iter()
        .find(|e| e.src == format!("file:{src}") && e.dst == dst && e.kind == kind)
        .map(|e| e.confidence)
}

fn file(p: &str) -> String {
    format!("file:{p}")
}

fn unresolved<'g>(g: &'g Graph, f: &str, spec: &str) -> Option<(&'g str, &'g str)> {
    g.unresolved
        .iter()
        .find(|u| u.file == f && u.specifier == spec)
        .map(|u| (u.reason.as_str(), u.detail.as_deref().unwrap_or("")))
}

/// The cases the golden file must keep.
fn pinned(g: &Graph) {
    use Confidence::{Exact, Heuristic, Resolved};
    use EdgeKind::{Call, Extends, Import, ImportExternal};
    let main = "game/scenes/main.tscn";
    // project.godot: main scene by uid, autoloads (script and scene), a setting.
    let proj = "game/project.godot";
    assert_eq!(edge(g, proj, &file(main), Import), Some(Resolved));
    assert_eq!(
        edge(g, proj, &file("game/autoload/events.gd"), Import),
        Some(Exact)
    );
    assert_eq!(
        edge(g, proj, &file("game/autoload/music.tscn"), Import),
        Some(Exact)
    );
    assert_eq!(
        edge(g, proj, &file("game/icon.svg"), Import),
        Some(Heuristic)
    );
    assert_eq!(
        unresolved(g, proj, "autoload Missing res://autoload/missing.gd").map(|u| u.0),
        Some("not_found")
    );
    let pmeta = &g.node(&file(proj)).unwrap().meta["godot"]["entry_points"];
    assert_eq!(pmeta[0]["kind"], "main_scene");
    assert_eq!(pmeta[0]["target"], file(main));
    // The scene: attached script (path and uid agree), instance, texture through the
    // .import uid, instance through the header uid when the path is stale.
    assert_eq!(
        edge(g, main, &file("game/scenes/main.gd"), Import),
        Some(Exact)
    );
    assert_eq!(
        edge(g, main, &file("game/scenes/player.tscn"), Import),
        Some(Exact)
    );
    assert_eq!(
        edge(g, main, &file("game/scenes/enemy.tscn"), Import),
        Some(Resolved)
    );
    assert_eq!(edge(g, main, &file("game/icon.svg"), Import), Some(Exact));
    assert!(
        unresolved(g, main, "Texture2D res://art/sprite.png")
            .is_some_and(|u| u.1.contains("skipped (binary)"))
    );
    assert!(unresolved(g, main, "AudioStream res://sfx/missing.ogg").is_some());
    // Signals: to the root's script and to an instanced scene's root script.
    assert_eq!(
        edge(g, main, &file("game/scenes/main.gd"), Call),
        Some(Resolved)
    );
    assert_eq!(
        edge(g, main, &file("game/scenes/player.gd"), Call),
        Some(Resolved)
    );
    let conns = g.node(&file(main)).unwrap().meta["godot"]["connections"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(conns.len(), 4);
    assert!(conns[2]["reason"].as_str().unwrap().contains("no script"));
    assert!(
        conns[3]["reason"]
            .as_str()
            .unwrap()
            .contains("not in this scene")
    );
    // Inheritance: class_name, extends path, built-in, inherited scene, typo.
    assert_eq!(
        edge(
            g,
            "game/scenes/player.gd",
            &file("game/scenes/actor.gd"),
            Extends
        ),
        Some(Resolved)
    );
    assert_eq!(
        edge(
            g,
            "game/scenes/enemy.gd",
            &file("game/scenes/actor.gd"),
            Extends
        ),
        Some(Exact)
    );
    assert_eq!(
        edge(
            g,
            "game/scenes/actor.gd",
            "external:godot:CharacterBody2D",
            ImportExternal
        ),
        Some(Exact)
    );
    assert_eq!(
        edge(
            g,
            "game/scenes/boss.tscn",
            &file("game/scenes/enemy.tscn"),
            Extends
        ),
        Some(Exact)
    );
    assert_eq!(
        unresolved(g, "game/scripts/typo.gd", "extends Nodee").map(|u| u.0),
        Some("not_found")
    );
    // Code: type hints and autoload names, preload by uid and relative path, dynamic loads,
    // ambiguous class, wrong case, string mentions, other project, outside any project.
    let gd = "game/scenes/main.gd";
    assert_eq!(
        edge(g, gd, &file("game/scenes/player.gd"), Import),
        Some(Resolved)
    );
    assert_eq!(
        edge(g, gd, &file("game/autoload/events.gd"), Import),
        Some(Resolved)
    );
    assert_eq!(
        edge(g, gd, &file("game/scenes/enemy.gd"), Import),
        Some(Exact)
    );
    assert_eq!(edge(g, gd, "dir:game/levels", Import), Some(Heuristic));
    assert_eq!(
        edge(
            g,
            "game/scenes/enemy.gd",
            &file("game/scenes/actor.gd"),
            Import
        ),
        Some(Exact)
    );
    assert_eq!(
        unresolved(g, gd, "load(level_dir + \"one.tres\")").map(|u| u.0),
        Some("dynamic")
    );
    assert_eq!(unresolved(g, gd, "Dup").map(|u| u.0), Some("ambiguous"));
    assert!(
        unresolved(g, gd, "load res://Scenes/Player.tscn")
            .is_some_and(|u| u.1.contains("differs only in case"))
    );
    assert_eq!(
        unresolved(g, gd, "load res://scenes/nope.tscn").map(|u| u.0),
        Some("not_found")
    );
    assert_eq!(
        edge(
            g,
            "game/scripts/tests.gd",
            &file("game/scripts/dup_a.gd"),
            Import
        ),
        Some(Heuristic)
    );
    assert_eq!(
        edge(g, "editor/main.gd", &file("editor/main.tscn"), Import),
        Some(Exact)
    );
    assert!(
        !g.edges
            .iter()
            .any(|e| e.src == file("editor/main.gd") && e.dst.starts_with("file:game/"))
    );
    assert!(
        unresolved(g, "loose.gd", "preload res://game/scenes/main.gd")
            .is_some_and(|u| u.1.contains("outside every Godot project"))
    );
    // Scripts outside every project still see each other's classes.
    assert_eq!(
        edge(
            g,
            "tools/loose_child.gd",
            &file("tools/loose_base.gd"),
            Extends
        ),
        Some(Resolved)
    );
    // An unknown uid is unresolved with a reason.
    assert!(
        unresolved(g, "game/scenes/enemy.gd", "preload uid://bnowhere0001")
            .is_some_and(|u| u.0 == "not_found" && u.1.contains("declares uid://bnowhere0001"))
    );
    // CRLF keeps line numbers.
    assert!(g.imports.iter().any(|s| s.file == "game/crlf/hud.tscn"
        && s.line == 3
        && s.target == file("game/crlf/hud.gd")));
    assert!(
        g.imports
            .iter()
            .any(|s| s.file == "game/crlf/hud.gd" && s.line == 3)
    );
    // Scenes and scripts roll up into folder dependencies on the plan.
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::DependsOn
        && e.src == "dir:game/scenes"
        && e.dst == "dir:game/autoload"));
    // The editor cache is never walked, and that is recorded.
    assert!(
        g.skipped.iter().any(
            |s| s.path == "game/.godot" && s.reason == cartograph_core::SkipReason::ExcludedDir
        )
    );
    assert!(
        g.node("file:game/.godot/global_script_class_cache.cfg")
            .is_none()
    );
}

#[test]
fn crlf_fixture_files_kept_their_line_endings() {
    for f in ["game/crlf/hud.tscn", "game/crlf/hud.gd"] {
        let bytes = fs::read(repo_root().join("tests/fixtures").join(FIXTURE).join(f)).unwrap();
        assert!(
            bytes.windows(2).any(|w| w == b"\r\n"),
            "{f} lost its CRLF line endings"
        );
    }
}

#[test]
fn cached_reindex_gives_the_same_graph() {
    let (_tmp, root) = copy_fixture();
    let data = tempfile::tempdir().unwrap();
    let opts = IndexOptions {
        data_dir: Some(data.path().to_path_buf()),
        ..IndexOptions::default()
    };
    let first = index_to_store(&root, &opts).unwrap();
    let second = index_to_store(&root, &opts).unwrap();
    assert_eq!(first.graph, second.graph);
    assert_eq!(second.timings.cache_misses, 0);
    assert!(second.timings.cache_hits > 20, "{:?}", second.timings);
    // Metadata survives the store.
    let store = Store::open(data.path().join("graph.db")).unwrap();
    assert_eq!(store.load_graph().unwrap(), second.graph);
    assert!(!db_path(&root).exists(), "nothing written inside the repo");
}

#[test]
fn autoloads_given_by_uid_resolve_and_register_their_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let write = |p: &str, t: &str| {
        let f = root.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, t).unwrap();
    };
    write(
        "project.godot",
        "config_version=5\n\n[autoload]\n\nEvents=\"*uid://bevents00001\"\n",
    );
    write("events.gd", "extends Node\n");
    write("events.gd.uid", "uid://bevents00001\n");
    write(
        "main.gd",
        "extends Node\n\nfunc _ready() -> void:\n\tEvents.go()\n",
    );
    let g = index(root, &IndexOptions::default()).unwrap().graph;
    assert_eq!(
        edge(&g, "project.godot", "file:events.gd", EdgeKind::Import),
        Some(Confidence::Resolved),
        "{:?}",
        g.unresolved
    );
    assert_eq!(
        edge(&g, "main.gd", "file:events.gd", EdgeKind::Import),
        Some(Confidence::Resolved)
    );
}
