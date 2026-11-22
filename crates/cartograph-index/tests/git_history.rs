//! The git layer on a synthetic repository (see `support/git_fixture.rs`): numbers match
//! `git log`, renames are followed, merges and huge commits are handled, passes are
//! incremental, rewritten history is rebuilt, and shallow, detached, empty and missing
//! repositories are reported without failing.

#[path = "support/git_fixture.rs"]
mod git_fixture;

use cartograph_core::{FileHistory, GitState, History, Store};
use cartograph_index::git;
use git_fixture::*;
use std::path::Path;

fn update(root: &Path, store: &mut Store) -> git::GitOutcome {
    git::update(root, store).unwrap()
}

fn fresh(root: &Path) -> History {
    let mut store = Store::open_in_memory().unwrap();
    update(root, &mut store);
    store.load_history().unwrap()
}

fn file<'a>(h: &'a History, path: &str) -> &'a FileHistory {
    h.files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} has no history: {:?}", h.files))
}

/// Everything but the pass bookkeeping (mode, commits read, time).
fn same_history(a: &History, b: &History) {
    assert_eq!(a.files, b.files);
    assert_eq!(a.pairs, b.pairs);
    let (sa, sb) = (a.status.clone().unwrap(), b.status.clone().unwrap());
    assert_eq!(
        (sa.head, sa.commits, sa.merges, sa.authors, sa.newest),
        (sb.head, sb.commits, sb.merges, sb.authors, sb.newest)
    );
}

#[test]
fn numbers_match_git_log() {
    let r = history_repo();
    let mut store = Store::open_in_memory().unwrap();
    let o = update(&r.root, &mut store);
    assert!(o.changed);
    let s = &o.status;
    assert_eq!(s.state, GitState::Ok);
    assert_eq!(s.mode, "full");
    assert_eq!((s.commits, s.merges), (24, 1));
    assert_eq!(s.scanned, 24);
    assert_eq!(s.authors, 2, "two people, four spellings");
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert!(!s.detached && !s.shallow);
    assert_eq!(s.prefix, "");
    let h = store.load_history().unwrap();

    // Churn of every file at HEAD equals `git log --no-merges --follow`.
    for f in &h.files {
        assert_eq!(
            f.commits as usize,
            r.log_count(&f.path, true),
            "commits of {}",
            f.path
        );
    }
    assert_eq!(h.files.len(), 6 + 60, "{:?}", h.files);
    // Lines added and removed equal `git log --numstat` (renames followed).
    for path in ["src/server.ts", "src/helpers.ts", "src/api.ts"] {
        let (mut add, mut del) = (0u64, 0u64);
        for line in r
            .git(&[
                "log",
                "--no-merges",
                "--follow",
                "--numstat",
                "--format=",
                "--",
                path,
            ])
            .lines()
        {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() == 3 {
                add += parts[0].parse::<u64>().unwrap();
                del += parts[1].parse::<u64>().unwrap();
            }
        }
        let f = file(&h, path);
        assert_eq!((f.added, f.removed), (add, del), "lines of {path}");
    }

    let client = file(&h, "src/client.ts");
    assert_eq!(client.commits, 11);
    assert_eq!(
        client.co_commits, 10,
        "the huge commit is left out of co-change"
    );
    assert_eq!(client.recent, 0);
    let names: Vec<(&str, u32)> = client
        .authors
        .iter()
        .map(|a| (a.name.as_str(), a.commits))
        .collect();
    assert_eq!(names, vec![("Ada Lovelace", 6), ("Grace Hopper", 5)]);
    // `git shortlog` groups by name only, so it splits Grace in two.
    let shortlog = r.git(&[
        "shortlog",
        "-sn",
        "--no-merges",
        "HEAD",
        "--",
        "src/client.ts",
    ]);
    assert_eq!(shortlog.lines().count(), 3, "{shortlog}");

    let server = file(&h, "src/server.ts");
    assert_eq!((server.commits, server.recent), (12, 1));
    assert_eq!(server.last_time, EPOCH + 200 * DAY);
    assert_eq!(server.first_time, EPOCH);

    // The rename is followed: util.ts's history is helpers.ts's.
    let helpers = file(&h, "src/helpers.ts");
    assert_eq!(helpers.commits, 3);
    assert!(helpers.complexity >= 3, "{helpers:?}");
    assert!(!h.files.iter().any(|f| f.path == "src/util.ts"));

    // Co-change: the pairs of the two groups, never through the huge commit.
    let pair = |a: &str, b: &str| {
        h.pairs
            .iter()
            .find(|p| p.a == a && p.b == b)
            .map(|p| p.together)
    };
    assert_eq!(pair("src/client.ts", "src/server.ts"), Some(10));
    assert_eq!(pair("src/api.ts", "src/db.ts"), Some(10));
    assert_eq!(pair("gen/f00.txt", "gen/f01.txt"), None);
    assert_eq!(
        pair("src/client.ts", "src/db.ts"),
        None,
        "one shared commit only"
    );
}

#[test]
fn passes_are_incremental_and_match_a_full_rebuild() {
    let r = history_repo();
    let mut store = Store::open_in_memory().unwrap();
    update(&r.root, &mut store);
    // Nothing new: nothing read, nothing written.
    let o = update(&r.root, &mut store);
    assert!(!o.changed && !o.read);
    assert_eq!(o.status.mode, "full");
    // Another branch on the same commit: the status changes, still nothing read.
    r.git(&["checkout", "-q", "-b", "side"]);
    let o = update(&r.root, &mut store);
    assert!(o.changed && !o.read, "{:?}", o.status);
    assert_eq!(o.status.branch.as_deref(), Some("side"));

    r.touch("src/client.ts", "// next");
    r.touch("src/server.ts", "// next");
    r.commit(ADA, 201, "next");
    std::fs::create_dir_all(r.root.join("src/lib")).unwrap();
    r.git(&["mv", "src/helpers.ts", "src/lib/helpers.ts"]);
    r.commit(GRACE, 202, "move helpers");
    let o = update(&r.root, &mut store);
    assert!(o.changed && o.read);
    assert_eq!(o.status.mode, "incremental");
    assert_eq!(o.status.scanned, 2);
    let h = store.load_history().unwrap();
    same_history(&h, &fresh(&r.root));
    assert_eq!(file(&h, "src/lib/helpers.ts").commits, 4, "renamed twice");
    assert_eq!(file(&h, "src/server.ts").recent, 2);
}

#[test]
fn rewritten_history_is_rebuilt() {
    let r = history_repo();
    let mut store = Store::open_in_memory().unwrap();
    update(&r.root, &mut store);
    // Drop the last two commits (the late fix and the merge, so the feature commit too)
    // and write another.
    r.git(&["reset", "-q", "--hard", "HEAD~2"]);
    r.touch("src/client.ts", "// rewritten");
    r.commit(ADA, 60, "rewritten");
    let o = update(&r.root, &mut store);
    assert_eq!(o.status.mode, "full", "the cursor is no longer an ancestor");
    let h = store.load_history().unwrap();
    same_history(&h, &fresh(&r.root));
    assert_eq!(store.load_git_raw().unwrap().commits.len(), 22);
    assert_eq!(file(&h, "src/server.ts").commits, 11);
    assert_eq!(
        file(&h, "src/db.ts").commits,
        9,
        "the feature commit is gone"
    );
}

#[test]
fn shallow_clones_are_partial_history() {
    let r = history_repo();
    let url = format!("file:///{}", r.root.to_str().unwrap().replace('\\', "/"));
    let url = url.replace("file:////", "file:///");
    let parent = r.root.parent().unwrap();
    r.git_in(parent, &["clone", "-q", "--depth", "3", &url, "shallow"]);
    let clone = parent.join("shallow");
    let mut store = Store::open_in_memory().unwrap();
    let o = update(&clone, &mut store);
    assert_eq!(o.status.state, GitState::Ok);
    assert!(o.status.shallow, "{:?}", o.status);
    let h = store.load_history().unwrap();
    // The edge of the clone counts as a root commit, as `git log` does there.
    let shallow_count = r.git_in(
        &clone,
        &["log", "--no-merges", "--format=%H", "--", "src/server.ts"],
    );
    assert_eq!(
        file(&h, "src/server.ts").commits as usize,
        shallow_count.lines().count()
    );
    assert!(file(&h, "src/server.ts").commits < 12);
}

/// `git fetch --deepen` and `--unshallow` keep HEAD where it was: the history must still
/// be read again, or it stays partial (and, once unshallowed, is no longer flagged).
#[test]
fn deepening_or_unshallowing_rebuilds_the_history() {
    let r = history_repo();
    let url = format!("file:///{}", r.root.to_str().unwrap().replace('\\', "/"));
    let url = url.replace("file:////", "file:///");
    let parent = r.root.parent().unwrap();
    r.git_in(parent, &["clone", "-q", "--depth", "2", &url, "shallow"]);
    let clone = parent.join("shallow");
    let count = |path: &str| {
        r.git_in(&clone, &["log", "--no-merges", "--format=%H", "--", path])
            .lines()
            .count()
    };
    let mut store = Store::open_in_memory().unwrap();
    let first = update(&clone, &mut store);
    assert!(first.status.shallow);

    r.git_in(&clone, &["fetch", "-q", "--deepen", "3"]);
    let o = update(&clone, &mut store);
    assert!(o.changed, "a new shallow boundary is new history");
    assert!(o.status.shallow);
    assert_eq!(o.status.mode, "full");
    assert!(o.status.commits > first.status.commits, "{:?}", o.status);
    let h = store.load_history().unwrap();
    assert_eq!(
        file(&h, "src/server.ts").commits as usize,
        count("src/server.ts")
    );
    same_history(&h, &fresh(&clone));

    r.git_in(&clone, &["fetch", "-q", "--unshallow"]);
    let o = update(&clone, &mut store);
    assert!(o.changed);
    assert!(!o.status.shallow);
    assert_eq!(o.status.mode, "full");
    let h = store.load_history().unwrap();
    same_history(&h, &fresh(&r.root));
    assert_eq!(file(&h, "src/server.ts").commits, 12);
    // And then nothing new.
    assert!(!update(&clone, &mut store).changed);
}

#[test]
fn detached_head_subfolders_and_missing_repositories() {
    let r = history_repo();
    // Detached on the merge: the late fix is not part of this history.
    r.git(&["checkout", "-q", "--detach", "HEAD~1"]);
    let mut store = Store::open_in_memory().unwrap();
    let o = update(&r.root, &mut store);
    assert!(o.status.detached);
    assert_eq!(o.status.branch, None);
    assert_eq!(
        file(&store.load_history().unwrap(), "src/server.ts").commits,
        11
    );

    // Indexing a subfolder: paths are relative to it, history outside is dropped.
    let mut store = Store::open_in_memory().unwrap();
    let o = update(&r.root.join("src"), &mut store);
    assert_eq!(o.status.prefix, "src");
    let h = store.load_history().unwrap();
    assert_eq!(file(&h, "client.ts").commits, 11);
    assert!(h.files.iter().all(|f| !f.path.starts_with("src/")));
    assert!(!h.files.iter().any(|f| f.path.starts_with("gen/")));

    // No repository at all.
    let plain = tempfile::tempdir().unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let o = update(plain.path(), &mut store);
    assert_eq!(o.status.state, GitState::NoGit);
    assert!(!o.changed, "no history before, none after");

    // A repository without commits.
    let empty = GitRepo::new();
    let mut store = Store::open_in_memory().unwrap();
    let o = update(&empty.root, &mut store);
    assert_eq!(o.status.state, GitState::NoCommits);
}

#[test]
fn emails_are_never_stored() {
    let r = history_repo();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph.db");
    {
        let mut store = Store::open(&db).unwrap();
        update(&r.root, &mut store);
        // Checkpoint the WAL into the main file before reading it raw.
        store
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
    }
    let mut bytes = Vec::new();
    for suffix in ["", "-wal"] {
        if let Ok(b) = std::fs::read(format!("{}{suffix}", db.display())) {
            bytes.extend(b);
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Ada Lovelace"));
    assert!(
        !text.contains("example.com"),
        "an email reached the database"
    );
}

/// A read that fails after a good one is a new status readers must see (the history rows
/// stay): `changed` once, not again while the same error repeats, and again once the
/// repository reads fine.
#[test]
fn a_failed_read_is_a_change_once() {
    let r = history_repo();
    let mut store = Store::open_in_memory().unwrap();
    let good = update(&r.root, &mut store);
    assert!(good.changed);
    let head = good.status.head.clone().unwrap();
    let main = r.root.join(".git/refs/heads/main");
    // HEAD now names a commit that does not exist.
    std::fs::write(&main, format!("{}\n", "1".repeat(40))).unwrap();
    let o = update(&r.root, &mut store);
    assert_eq!(o.status.state, GitState::Error, "{:?}", o.status);
    assert!(o.changed, "Ok -> Error must be published");
    let h = store.load_history().unwrap();
    assert_eq!(h.status.as_ref().unwrap().state, GitState::Error);
    assert_eq!(file(&h, "src/server.ts").commits, 12, "rows kept");
    assert!(
        !update(&r.root, &mut store).changed,
        "the same error again is nothing new"
    );
    std::fs::write(&main, format!("{head}\n")).unwrap();
    let o = update(&r.root, &mut store);
    assert_eq!(o.status.state, GitState::Ok);
    assert!(o.changed);
}

/// A new branch at the same commit (`git switch -c`) changes only the flags: the status
/// readers see changed, so the pass says so.
#[test]
fn a_new_branch_at_the_same_commit_is_a_change() {
    let r = history_repo();
    let mut store = Store::open_in_memory().unwrap();
    update(&r.root, &mut store);
    r.git(&["switch", "-q", "-c", "topic"]);
    let o = update(&r.root, &mut store);
    assert!(o.changed, "the branch changed");
    assert_eq!(o.status.branch.as_deref(), Some("topic"));
    assert_eq!(
        store
            .load_history()
            .unwrap()
            .status
            .unwrap()
            .branch
            .as_deref(),
        Some("topic")
    );
    assert!(!update(&r.root, &mut store).changed);
    r.git(&["checkout", "-q", "--detach"]);
    let o = update(&r.root, &mut store);
    assert!(o.changed, "detached");
    assert!(o.status.detached);
}
