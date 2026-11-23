//! Workspaces (`tests/fixtures/workspace-basic`): each member walked from its own folder
//! and mapped under its name, nothing else of the parent walked, imports resolved across
//! members, one git history per member.
//!
//! The graph is compared with `tests/golden/workspace-basic.json`; regenerate with
//! `UPDATE_GOLDEN=1 cargo test -p cartograph-index --test workspace` and review the diff.

#[path = "support/git_fixture.rs"]
mod git_fixture;
#[path = "support/workspace_fixture.rs"]
mod workspace_fixture;

use cartograph_core::{EdgeKind, GitState, GitStatus, Graph, NodeKind, Store};
use cartograph_index::{IndexOptions, git, index_to_store};
use git_fixture::{ADA, GRACE};
use serde_json::{Value, json};
use std::path::Path;
use workspace_fixture::{MEMBERS, workspace_basic};

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn index(root: &Path, data: &Path) -> (Graph, Store) {
    let opts = IndexOptions {
        data_dir: Some(data.to_path_buf()),
        ..IndexOptions::default()
    };
    let out = index_to_store(root, &opts).unwrap();
    let store = Store::open(data.join(cartograph_index::DB_FILE)).unwrap();
    (out.graph, store)
}

/// The graph as sorted JSON rows: nodes (no hashes), dependency edges, import sites,
/// unresolved imports and skipped paths.
fn golden_view(g: &Graph) -> Value {
    let nodes: Vec<Value> = g
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::File || n.lang.is_some())
        .map(|n| {
            let member = &n.meta["member"];
            json!([n.id, n.name, n.loc, member])
        })
        .collect();
    let edges: Vec<Value> = g
        .edges
        .iter()
        .filter(|e| e.kind != EdgeKind::Contains || e.src.starts_with("package:"))
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
    let skipped: Vec<Value> = g
        .skipped
        .iter()
        .map(|s| json!([s.path, s.reason, s.detail]))
        .collect();
    json!({
        "nodes": nodes,
        "edges": edges,
        "sites": sites,
        "unresolved": unresolved,
        "skipped": skipped,
    })
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

fn edge(g: &Graph, src: &str, dst: &str) -> bool {
    g.edges.iter().any(|e| {
        e.src == format!("file:{src}")
            && e.dst == format!("file:{dst}")
            && e.kind != EdgeKind::Contains
    })
}

#[test]
fn members_are_top_level_folders_and_nothing_else_is_walked() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let (g, _) = index(&ws.root, data.path());
    check_golden("workspace-basic", &golden_view(&g));

    let top: Vec<&str> = g
        .nodes_of(NodeKind::Dir)
        .filter(|n| !n.path.contains('/'))
        .map(|n| n.path.as_str())
        .collect();
    let mut members: Vec<&str> = MEMBERS.iter().map(|(m, _)| *m).collect();
    members.sort();
    assert_eq!(top, members, "only members at the top");
    for (name, folder) in MEMBERS {
        let dir = g.node(&format!("dir:{name}")).unwrap();
        assert_eq!(dir.meta["member"], json!({ "name": name, "path": folder }));
    }
    let repo = g.node("repo:").unwrap();
    assert_eq!(repo.name, "system");
    for n in &g.nodes {
        for noise in [
            "logs",
            "backups",
            "docs",
            "api-worktrees",
            "shared-lib",
            "backend-api",
        ] {
            assert!(
                !n.path.starts_with(&format!("{noise}/")),
                "{} was walked",
                n.path
            );
        }
    }
    // Imports across members: Java by package, TypeScript by a relative path that leaves
    // the member's folder (`../../desktop-app/...`, the folder of member `desktop`).
    assert!(edge(
        &g,
        "api/src/main/java/com/acme/api/UserService.java",
        "lib/src/main/java/com/acme/lib/UserDto.java"
    ));
    assert!(edge(
        &g,
        "mcp/src/tools.ts",
        "desktop/src/shared/channels.ts"
    ));
    // A package at a member's root is named by its manifest, else after the member's folder.
    let pkg = g.node("package:lib/pom.xml").unwrap();
    assert_eq!(pkg.meta["dir"], "lib");
}

#[test]
fn each_member_has_its_own_history() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let (_, mut store) = index(&ws.root, data.path());
    let out = git::update(&ws.root, &mut store).unwrap();
    assert!(out.changed && out.read);
    assert_eq!(out.status.state, GitState::Ok);
    assert_eq!(out.status.commits, 5, "one per member, two in api");
    let member = |s: &Store, m: &str| -> GitStatus {
        serde_json::from_str(&s.get_meta(&format!("git:{m}")).unwrap().unwrap()).unwrap()
    };
    let api = member(&store, "api");
    assert_eq!(api.state, GitState::Ok);
    assert_eq!(api.branch.as_deref(), Some("main"));
    assert_eq!(api.commits, 2);
    assert_eq!(member(&store, "lib").commits, 1);
    let h = store.load_history().unwrap();
    let file = |p: &str| {
        h.files
            .iter()
            .find(|f| f.path == p)
            .unwrap_or_else(|| panic!("{p}"))
    };
    let controller = "api/src/main/java/com/acme/api/UserController.java";
    assert_eq!(file(controller).commits, 2);
    assert_eq!(file("lib/pom.xml").commits, 1);
    assert!(
        h.pairs
            .iter()
            .any(|p| p.a.starts_with("api/") && p.b.starts_with("api/")),
        "co-change inside the api"
    );
    assert!(h.files.iter().all(|f| {
        MEMBERS
            .iter()
            .any(|(m, _)| f.path.starts_with(&format!("{m}/")))
    }));

    // Nothing new: nothing read. A new commit in one member: only that member is read.
    let again = git::update(&ws.root, &mut store).unwrap();
    assert!(!again.changed && !again.read);
    let lib = ws.repo("lib");
    lib.touch("src/main/java/com/acme/lib/Money.java", "// more");
    lib.commit(GRACE, 5, "money");
    let raw_before: Vec<_> = store
        .load_git_raw()
        .unwrap()
        .commits
        .into_iter()
        .filter(|c| c.member == "api")
        .collect();
    let out = git::update(&ws.root, &mut store).unwrap();
    assert!(out.changed && out.read);
    assert_eq!(member(&store, "lib").mode, "incremental");
    assert_eq!(member(&store, "lib").commits, 2);
    assert_eq!(out.status.commits, 6);
    let raw_after: Vec<_> = store
        .load_git_raw()
        .unwrap()
        .commits
        .into_iter()
        .filter(|c| c.member == "api")
        .collect();
    assert_eq!(raw_before, raw_after, "api's rows untouched");
    let h = store.load_history().unwrap();
    let money = h
        .files
        .iter()
        .find(|f| f.path == "lib/src/main/java/com/acme/lib/Money.java")
        .unwrap();
    assert_eq!(money.commits, 2);
    assert_eq!(money.authors.len(), 2, "{:?} {ADA:?}", money.authors);
}

#[test]
fn a_member_without_git_is_reported_and_the_others_are_read() {
    let ws = workspace_basic();
    std::fs::remove_dir_all(ws.root.join("mcp/.git")).unwrap();
    let data = tempfile::tempdir().unwrap();
    let (_, mut store) = index(&ws.root, data.path());
    let out = git::update(&ws.root, &mut store).unwrap();
    assert_eq!(out.status.state, GitState::Ok);
    let mcp: GitStatus =
        serde_json::from_str(&store.get_meta("git:mcp").unwrap().unwrap()).unwrap();
    assert_eq!(mcp.state, GitState::NoGit);
}

#[test]
fn a_bad_workspace_file_is_an_error_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(cartograph_core::WORKSPACE_FILE),
        "[[member]]\nname = \"npm\"\npath = \"x\"\n",
    )
    .unwrap();
    let e = cartograph_index::index(tmp.path(), &IndexOptions::default()).unwrap_err();
    assert!(e.to_string().contains("reserved"), "{e}");
    // A member folder that does not exist.
    std::fs::write(
        tmp.path().join(cartograph_core::WORKSPACE_FILE),
        "[[member]]\nname = \"web\"\npath = \"nope\"\n",
    )
    .unwrap();
    let e = cartograph_index::index(tmp.path(), &IndexOptions::default()).unwrap_err();
    assert!(e.to_string().contains("nope"), "{e}");
}

#[test]
fn a_member_folder_alone_is_a_plain_repo() {
    let ws = workspace_basic();
    let out =
        cartograph_index::index(&ws.root.join("backend-api"), &IndexOptions::default()).unwrap();
    assert!(
        out.graph
            .node("file:src/main/java/com/acme/api/UserController.java")
            .is_some()
    );
    assert!(out.graph.node("dir:api").is_none());
}

/// Every commit names an author that is stored.
fn authors_complete(store: &Store) {
    let raw = store.load_git_raw().unwrap();
    let ids: std::collections::HashSet<u32> = raw.authors.iter().map(|a| a.id).collect();
    for c in &raw.commits {
        assert!(
            ids.contains(&c.author),
            "commit {} names a missing author",
            c.id
        );
    }
}

#[test]
fn a_member_rebuilt_from_scratch_keeps_its_authors() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let (_, mut store) = index(&ws.root, data.path());
    git::update(&ws.root, &mut store).unwrap();
    authors_complete(&store);
    // A rewritten history (the cursor is no ancestor of HEAD any more): a full rebuild of
    // that member alone, by the same people.
    let api = ws.repo("api");
    api.git(&["reset", "-q", "--hard", "HEAD~1"]);
    api.touch("src/main/java/com/acme/api/UserService.java", "// again");
    api.commit(GRACE, 4, "review again");
    let out = git::update(&ws.root, &mut store).unwrap();
    assert!(out.read);
    let api_status: GitStatus =
        serde_json::from_str(&store.get_meta("git:api").unwrap().unwrap()).unwrap();
    assert_eq!(api_status.mode, "full");
    authors_complete(&store);
    let h = store.load_history().unwrap();
    let service = h
        .files
        .iter()
        .find(|f| f.path == "api/src/main/java/com/acme/api/UserService.java")
        .unwrap();
    assert_eq!(service.authors.len(), 2, "{:?}", service.authors);
}

#[test]
fn people_are_counted_per_member_and_once_in_the_summary() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let (_, mut store) = index(&ws.root, data.path());
    let out = git::update(&ws.root, &mut store).unwrap();
    let member = |m: &str| -> GitStatus {
        serde_json::from_str(&store.get_meta(&format!("git:{m}")).unwrap().unwrap()).unwrap()
    };
    assert_eq!(member("lib").authors, 1, "only Ada committed to lib");
    assert_eq!(member("api").authors, 2);
    assert_eq!(out.status.authors, 2, "Ada and Grace, once each");
}

#[test]
fn a_member_removed_then_added_again_is_read_again() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let file = ws.root.join(cartograph_core::WORKSPACE_FILE);
    let full = std::fs::read_to_string(&file).unwrap();
    let (_, mut store) = index(&ws.root, data.path());
    git::update(&ws.root, &mut store).unwrap();
    // Without mcp: its rows and keys go.
    let without = full.replace("[[member]]\nname = \"mcp\"\npath = \"mcp\"\n", "");
    assert_ne!(without, full);
    std::fs::write(&file, &without).unwrap();
    git::update(&ws.root, &mut store).unwrap();
    for k in ["git:mcp", "git_cursor:mcp", "git_scope:mcp"] {
        assert_eq!(store.get_meta(k).unwrap(), None, "{k}");
    }
    assert!(!store.git_members().unwrap().contains(&"mcp".to_string()));
    // Back, HEAD unchanged: its history is read again.
    std::fs::write(&file, &full).unwrap();
    git::update(&ws.root, &mut store).unwrap();
    let h = store.load_history().unwrap();
    assert!(
        h.files.iter().any(|f| f.path == "mcp/src/tools.ts"),
        "{:?}",
        h.files
    );
}

#[test]
fn a_single_repo_over_a_workspace_store_starts_over() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let (_, mut store) = index(&ws.root, data.path());
    git::update(&ws.root, &mut store).unwrap();
    // The same data dir, now for the API alone.
    let out = git::update(&ws.root.join("backend-api"), &mut store).unwrap();
    assert!(out.read);
    assert_eq!(out.status.commits, 2);
    assert_eq!(store.git_members().unwrap(), vec![String::new()]);
    assert_eq!(store.get_meta("git:api").unwrap(), None);
    let h = store.load_history().unwrap();
    assert!(
        h.files
            .iter()
            .any(|f| f.path == "src/main/java/com/acme/api/UserService.java")
    );
    assert!(h.files.iter().all(|f| !f.path.starts_with("api/")));
    authors_complete(&store);
}
