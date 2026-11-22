//! The engine serves the file graph before it reads git history, and a commit that changes
//! no working-tree file still refreshes that history (only the history: no file is read
//! again), in a linked worktree too, whose git files live outside the indexed root.

#[path = "../../cartograph-index/tests/support/git_fixture.rs"]
mod git_fixture;

use cartograph_mcp::engine::{Engine, Loaded, Options};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn engine(root: &Path, data: &Path, watch: bool) -> Arc<Engine> {
    let mut opts = Options::new(root);
    opts.data_dir = Some(data.to_path_buf());
    opts.watch = watch;
    let eng = Engine::new(opts).unwrap();
    eng.start();
    eng
}

/// The first state that satisfies `pred` (panics after 60 s).
async fn wait_for(eng: &Engine, what: &str, pred: impl Fn(&Loaded) -> bool) -> Arc<Loaded> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(l) = eng.current().filter(|l| pred(l)) {
            return l;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn head(l: &Loaded) -> Option<String> {
    l.repo.git.status.as_ref().and_then(|s| s.head.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_file_graph_comes_first_then_one_more_generation_with_the_history() {
    let repo = git_fixture::history_repo();
    let data = tempfile::tempdir().unwrap();
    let eng = engine(&repo.root, data.path(), false);
    let l = wait_for(&eng, "the history", |l| l.repo.git.indexed()).await;
    assert_eq!(
        l.generation, 2,
        "generation 1 is the file graph (without history), generation 2 adds it"
    );
    assert!(!l.from_disk);
    eng.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_in_a_linked_worktree_refreshes_only_the_history() {
    let repo = git_fixture::history_repo();
    let wt = repo.root.parent().unwrap().join("wt");
    repo.git(&["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
    let data = tempfile::tempdir().unwrap();
    let eng = engine(&wt, data.path(), true);
    let first = wait_for(&eng, "the history", |l| l.repo.git.indexed()).await;

    // Edit, and let the index take it in.
    let file = wt.join("src/server.ts");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("// edited\n");
    std::fs::write(&file, &text).unwrap();
    let bytes = text.len() as u64;
    wait_for(&eng, "the edit", |l| {
        l.files.get("src/server.ts").map(|f| f.0) == Some(bytes)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let edited = eng.current().unwrap();

    // Commit it: only git's files change (here outside the indexed root).
    repo.git_in(&wt, &["add", "-A"]);
    repo.git_in(&wt, &["commit", "-q", "-m", "commit the edit"]);
    let committed = wait_for(&eng, "the new commit", |l| head(l) != head(&edited)).await;
    assert_ne!(head(&committed), head(&first));
    assert!(committed.generation > edited.generation);
    assert_eq!(
        committed.started, edited.started,
        "a commit alone reads the history again, not the files"
    );
    eng.cancel();
}
