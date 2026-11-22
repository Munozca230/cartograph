//! The server reads git history after it serves the file graph: the first snapshot comes
//! without history, the next one (one more generation) carries the hidden-coupling finding
//! to `/api/findings`; a re-index with no new commit keeps it and adds no extra snapshot;
//! a commit that changes no working-tree file refreshes the history alone.

mod common;
#[path = "../../cartograph-index/tests/support/git_fixture.rs"]
mod git_fixture;

use cartograph_server::indexer::{IndexFn, IndexRequest, default_index_fn};
use cartograph_server::{ServeConfig, start, start_with};
use common::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn hidden(findings: &serde_json::Value) -> Vec<String> {
    findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "hidden_coupling")
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hidden_coupling_reaches_the_findings_endpoint_after_the_first_paint() {
    let repo = git_fixture::history_repo();
    let data = tempfile::tempdir().unwrap();
    let server = start(ServeConfig {
        port: Some(0),
        watch: false,
        data_dir: Some(data.path().to_path_buf()),
        ..ServeConfig::new(&repo.root)
    })
    .await
    .unwrap();
    let addr = server.addr;
    let found = wait_for("hidden coupling in /api/findings", || async move {
        let (status, body) = http_json(addr, "/api/findings").await;
        (status == 200 && !hidden(&body).is_empty()).then_some(body)
    })
    .await;
    assert_eq!(
        hidden(&found),
        vec!["hidden_coupling:file:src/client.ts->file:src/server.ts"]
    );
    let g = settle(addr, Duration::from_millis(800)).await;
    assert_eq!(
        g, 2,
        "the file graph first, then the graph with its history"
    );

    // No new commit: the history comes from the store with the first snapshot.
    let mut events = sse(addr).await;
    let (status, _) = http(addr, "POST", "/api/reindex").await;
    assert_eq!(status, 202);
    next_event(&mut events, |e| e.event == "index-finished").await;
    let g = settle(addr, Duration::from_millis(800)).await;
    assert_eq!(g, 3, "no second snapshot when the history did not change");
    let (_, body) = http_json(addr, "/api/findings").await;
    assert_eq!(hidden(&body).len(), 1);
    server.stop().await;
}

/// Files edited, indexed, then committed: the commit changes no working-tree file, only
/// git's (`HEAD`'s branch ref, the reflog). The history is read again (one more
/// generation, an `index-finished` marked `git`), and no file is indexed again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commit_of_indexed_edits_refreshes_only_the_history() {
    let repo = git_fixture::history_repo();
    let data = tempfile::tempdir().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = runs.clone();
    let inner = default_index_fn();
    let counting: IndexFn = Arc::new(move |req: &IndexRequest| {
        counted.fetch_add(1, Ordering::SeqCst);
        inner(req)
    });
    let server = start_with(
        ServeConfig {
            port: Some(0),
            watch: true,
            data_dir: Some(data.path().to_path_buf()),
            ..ServeConfig::new(&repo.root)
        },
        counting,
    )
    .await
    .unwrap();
    let addr = server.addr;
    wait_for("the history", || async move {
        let (status, body) = http_json(addr, "/api/findings").await;
        (status == 200 && !hidden(&body).is_empty()).then_some(())
    })
    .await;
    settle(addr, Duration::from_millis(800)).await;

    let mut events = sse(addr).await;
    repo.touch("src/server.ts", "// edited");
    next_event(&mut events, |e| {
        e.event == "index-finished" && e.data["changed"].to_string().contains("src/server.ts")
    })
    .await;
    let g = settle(addr, Duration::from_millis(1500)).await;
    let before = runs.load(Ordering::SeqCst);

    repo.commit(git_fixture::ADA, 300, "commit the edit");
    let ev = next_event(&mut events, |e| {
        e.event == "index-finished" && e.data["git"] == true
    })
    .await;
    assert!(ev.data["generation"].as_u64().unwrap() > g, "{:?}", ev.data);
    settle(addr, Duration::from_millis(1000)).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        before,
        "a commit alone reads no file again"
    );
    server.stop().await;
}
