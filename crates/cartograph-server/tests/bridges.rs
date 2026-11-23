//! Bridges over HTTP (`tests/fixtures/electron-app`, a single Electron repo): `/api/graph`
//! carries them in their own `bridges` array (the weights of `edges` still sum to the
//! imports), `/api/bridges` lists them, and a file's detail names its own.

mod common;

use cartograph_core::{EdgeKind, History};
use cartograph_index::IndexOptions;
use cartograph_server::api;
use cartograph_server::snapshot::Snapshot;
use cartograph_server::state::AppState;
use common::*;
use std::collections::BTreeMap;
use std::sync::Arc;

fn state() -> Arc<AppState> {
    let root = fixture("electron-app");
    let out = cartograph_index::index(&root, &IndexOptions::default()).unwrap();
    let mut meta = BTreeMap::new();
    meta.insert("repo_name".to_string(), "electron-app".to_string());
    let snap = Snapshot::build_all(
        out.graph,
        &meta,
        1,
        Ok(cartograph_query::Config::default()),
        &History::default(),
        &out.symbols,
    );
    let state = Arc::new(AppState::new(
        "electron-app".to_string(),
        root.clone(),
        root.join(".x"),
    ));
    state.set_port(PORT);
    state.set_snapshot(Arc::new(snap));
    state
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridges_have_their_own_array_and_endpoint() {
    let app = api::router(state());
    let (status, g) = call(&app, "GET", "/api/graph?depth=all").await;
    assert_eq!(status, 200, "{g}");
    let bridges = g["bridges"].as_array().unwrap();
    assert_eq!(bridges.len(), 1, "{g}");
    assert_eq!(bridges[0]["src"], "file:src/electron/preload.ts");
    assert_eq!(bridges[0]["dst"], "file:src/electron/main.ts");
    assert_eq!(bridges[0]["weight"], 3);
    // The import edges are as without bridges: their weights sum to the imports between
    // distinct cards (every card is a file here).
    let out = cartograph_index::index(&fixture("electron-app"), &IndexOptions::default()).unwrap();
    let imports = out
        .graph
        .edges
        .iter()
        .filter(|e| {
            e.kind == EdgeKind::Import && e.src.starts_with("file:") && e.dst.starts_with("file:")
        })
        .count() as u64;
    let sum: u64 = g["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["weight"].as_u64().unwrap())
        .sum();
    assert_eq!(sum, imports, "{g}");

    let (status, b) = call(&app, "GET", "/api/bridges").await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(b["total"], 3, "{b}");
    let keys: Vec<&str> = b["bridges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"doc:save"), "{b}");
    let unmatched: Vec<&str> = b["unmatched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["key"].as_str().unwrap())
        .collect();
    assert!(unmatched.contains(&"update:now"), "{b}");
    let (status, b) = call(&app, "GET", "/api/bridges?node=dir:src/ui").await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(b["total"], 0, "{b}");
    let (status, _) = call(&app, "GET", "/api/bridges?node=file:nope.ts").await;
    assert_eq!(status, 404);

    let (status, d) = call(&app, "GET", "/api/node?id=file:src/electron/main.ts").await;
    assert_eq!(status, 200, "{d}");
    assert_eq!(d["bridges"].as_array().unwrap().len(), 3, "{d}");
}
