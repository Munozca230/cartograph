//! Base snapshots (`git::snapshot`) on synthetic repositories (see `support/git_fixture.rs`):
//! what the base holds compared with the working tree, how a revision is labelled, and when
//! a cached snapshot is reused.

#[path = "support/git_fixture.rs"]
mod git_fixture;

use cartograph_index::IndexOptions;
use cartograph_index::git::snapshot;
use git_fixture::{ADA, GitRepo};
use std::path::Path;
use std::time::{Duration, SystemTime};

fn opts(data: &Path) -> IndexOptions {
    IndexOptions {
        data_dir: Some(data.to_path_buf()),
        ..IndexOptions::default()
    }
}

/// The cached snapshot files of a data dir.
fn cached(data: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(data.join(snapshot::SNAPSHOT_DIR))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
}

/// A cache hit marks the snapshot as recently used, so pruning (by modification time)
/// keeps the snapshots in use, not the ones built last.
#[test]
fn a_cache_hit_refreshes_the_snapshot_time() {
    let r = GitRepo::new();
    r.write("src/a.ts", "export const a = 1;\n");
    r.commit(ADA, 0, "base");
    let data = tempfile::tempdir().unwrap();
    let first = snapshot::snapshot(&r.root, "HEAD", &opts(data.path())).unwrap();
    assert!(!first.cached);
    let files = cached(data.path());
    assert_eq!(files.len(), 1, "{files:?}");
    let old = SystemTime::now() - Duration::from_secs(30 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&files[0])
        .unwrap()
        .set_modified(old)
        .unwrap();

    let again = snapshot::snapshot(&r.root, "HEAD", &opts(data.path())).unwrap();
    assert!(again.cached);
    let modified = std::fs::metadata(&files[0]).unwrap().modified().unwrap();
    assert!(
        modified > old + Duration::from_secs(86_400),
        "the hit did not touch the snapshot: {modified:?}"
    );
}

fn file_paths(g: &cartograph_core::Graph) -> Vec<String> {
    g.nodes_of(cartograph_core::NodeKind::File)
        .map(|n| n.path.clone())
        .collect()
}

/// An initialized submodule is a gitlink in the base (left out) and a checkout with a
/// `.git` file in the working tree: the walk leaves it out too, so `diff` does not show its
/// files as added.
#[test]
fn submodule_checkouts_are_left_out_on_both_sides() {
    let sub = GitRepo::new();
    sub.write("lib/vendored.ts", "export const v = 1;\n");
    sub.commit(ADA, 0, "sub");
    let r = GitRepo::new();
    r.write("src/a.ts", "export const a = 1;\n");
    let url = sub.root.to_string_lossy().replace('\\', "/");
    r.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        &url,
        "deps/sub",
    ]);
    r.commit(ADA, 1, "base with a submodule");
    assert!(r.root.join("deps/sub/lib/vendored.ts").is_file());
    assert!(r.root.join("deps/sub/.git").is_file());

    let data = tempfile::tempdir().unwrap();
    let head = cartograph_index::index(&r.root, &opts(data.path())).unwrap();
    let base = snapshot::snapshot(&r.root, "HEAD", &opts(data.path())).unwrap();
    assert_eq!(file_paths(&head.graph), file_paths(&base.graph));
    assert!(
        !file_paths(&head.graph)
            .iter()
            .any(|p| p.starts_with("deps/sub/")),
        "{:?}",
        file_paths(&head.graph)
    );
    let skipped = head
        .graph
        .skipped
        .iter()
        .find(|s| s.path == "deps/sub")
        .expect("the submodule is recorded as skipped");
    assert_eq!(skipped.reason, cartograph_core::SkipReason::ExcludedDir);
}

/// A folder holding a `.git` file (a linked worktree, a submodule checkout) is another
/// repository's working tree: it is skipped whole, but a `.git` file at the root (this
/// repository is itself a linked worktree) is not.
#[test]
fn folders_with_a_git_file_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let write = |p: &str, text: &str| {
        let p = root.join(p);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write(".git", "gitdir: /elsewhere/.git/worktrees/x\n");
    write("src/a.ts", "export const a = 1;\n");
    write("nested/.git", "gitdir: ../.git/modules/nested\n");
    write("nested/b.ts", "export const b = 1;\n");
    let out = cartograph_index::index(root, &IndexOptions::default()).unwrap();
    assert_eq!(file_paths(&out.graph), vec!["src/a.ts".to_string()]);
}

/// A branch whose name happens to be a prefix of its commit id keeps its name in the label;
/// a commit id (full or short) is shown short.
#[test]
fn a_branch_named_like_its_commit_keeps_its_name() {
    let r = GitRepo::new();
    r.write("src/a.ts", "export const a = 1;\n");
    r.commit(ADA, 0, "base");
    let hash = r.git(&["rev-parse", "HEAD"]).trim().to_string();
    let short = &hash[..8];
    let name = &hash[..6];
    r.git(&["branch", name]);

    let by_name = snapshot::resolve(&r.root, name).unwrap();
    assert_eq!(by_name.commit, hash);
    assert_eq!(by_name.label(), format!("{name} ({short})"));
    for id in [hash.as_str(), &hash[..10]] {
        assert_eq!(snapshot::resolve(&r.root, id).unwrap().label(), short);
    }
    assert_eq!(
        snapshot::resolve(&r.root, "main").unwrap().label(),
        format!("main ({short})")
    );
}

fn unresolved(s: &snapshot::Snapshot) -> Vec<String> {
    s.graph
        .unresolved
        .iter()
        .map(|u| format!("{} {}", u.file, u.specifier))
        .collect()
}

/// A cached snapshot is reused only while the working-tree files its build read are
/// unchanged: an untracked `compile_commands.json` (read from disk, for the base too) and
/// the `.gitattributes` of the worktree conversion.
#[test]
fn a_cached_snapshot_is_rebuilt_when_the_files_it_read_change() {
    let r = GitRepo::new();
    r.write(
        "src/main.c",
        "#include \"util.h\"\nint main(void) { return 0; }\n",
    );
    r.write("a/util.h", "int a;\n");
    r.write("b/util.h", "int b;\n");
    r.commit(ADA, 0, "base");
    let data = tempfile::tempdir().unwrap();
    let build = || snapshot::snapshot(&r.root, "HEAD", &opts(data.path())).unwrap();

    let first = build();
    assert!(!first.cached);
    assert_eq!(unresolved(&first), vec!["src/main.c util.h".to_string()]);
    assert!(build().cached);

    // Untracked: a checkout of the base would see it, so the base must.
    r.write(
        "compile_commands.json",
        r#"[{"directory": ".", "file": "src/main.c", "arguments": ["cc", "-Ia", "-c", "src/main.c"]}]"#,
    );
    let with_db = build();
    assert!(!with_db.cached, "the compilation database was not seen");
    assert!(
        unresolved(&with_db).is_empty(),
        "{:?}",
        unresolved(&with_db)
    );
    assert!(build().cached);

    r.write(".gitattributes", "*.h text eol=crlf\n");
    assert!(!build().cached, "the new .gitattributes was not seen");
    assert!(build().cached);
}
