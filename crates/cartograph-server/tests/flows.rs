//! The flow endpoints: `/api/entries`, `/api/flow`, `/api/symbol`,
//! `/api/file-flows` and functions in `/api/search?kinds=sym`.

mod common;

use cartograph_core::History;
use cartograph_index::IndexOptions;
use cartograph_query::Config;
use cartograph_server::snapshot::Snapshot;
use cartograph_server::state::AppState;
use cartograph_server::{ServeConfig, api, start};
use common::*;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// App state serving the `flow-rust` fixture with its symbol layer.
fn flow_state() -> Arc<AppState> {
    let root = fixture("flow-rust");
    let out = cartograph_index::index(&root, &IndexOptions::default()).unwrap();
    let mut meta = BTreeMap::new();
    meta.insert("repo_name".to_string(), "flow-rust".to_string());
    let state = Arc::new(AppState::new(
        "flow-rust".to_string(),
        root.clone(),
        root.join(".x"),
    ));
    state.set_port(PORT);
    state.set_snapshot(Arc::new(Snapshot::build_all(
        out.graph,
        &meta,
        7,
        Ok(Config::default()),
        &History::default(),
        &out.symbols,
    )));
    state
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|x| x[key].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn entries_list_flows_outside_test_trees() {
    let app = api::router(flow_state());
    let (status, e) = get(&app, "/api/entries").await;
    assert_eq!(status, 200, "{e}");
    assert_eq!(e["generation"], 7);
    assert_eq!(e["indexed"], true);
    assert_eq!(e["hidden"], 1);
    assert_eq!(
        names(&e["entries"], "name"),
        vec!["index", "serve", "GET /health", "main"]
    );
    assert_eq!(e["entries"][0]["kind"], "cli");
    assert_eq!(e["entries"][0]["root"], "sym:src/main.rs#cmd_index");
    assert_eq!(e["entries"][0]["file"], "src/main.rs");
    assert_eq!(e["entries"][0]["line"], 11);
}

#[tokio::test]
async fn flow_is_bounded_and_expandable() {
    let app = api::router(flow_state());
    let (status, f) = get(&app, "/api/flow?from=sym:src/main.rs%23cmd_index").await;
    assert_eq!(status, 200, "{f}");
    assert_eq!(f["generation"], 7);
    assert_eq!(f["indexed"], true);
    assert_eq!(f["root"], "sym:src/main.rs#cmd_index");
    assert_eq!(f["depth"], 4);
    assert_eq!(f["truncated"], true);
    let nodes = f["nodes"].as_array().unwrap();
    let run_all = nodes.iter().find(|n| n["name"] == "run_all").unwrap();
    assert_eq!(run_all["more"], 3);
    assert_eq!(run_all["file"], "src/steps.rs");
    assert_eq!(run_all["lane"], "src");
    assert_eq!(run_all["finding"], "warning");
    assert!(run_all["callers"].as_u64().unwrap() >= 1);
    let main = nodes.iter().find(|n| n["name"] == "cmd_index").unwrap();
    assert!(main.get("finding").is_none());
    let edge = &f["edges"][0];
    assert_eq!(edge["src"], "sym:src/main.rs#cmd_index");
    assert_eq!(edge["confidence"], "resolved");
    assert_eq!(edge["line"], 27);

    // Expanded, repeated or comma-separated: run_all shows all its callees.
    for q in [
        "expand=sym:src/steps.rs%23run_all",
        "expand=sym:src/steps.rs%23run_all,sym:src/steps.rs%23chain3",
        "expand=sym:src/steps.rs%23run_all&expand=sym:src/steps.rs%23chain3",
    ] {
        let (status, f) = get(
            &app,
            &format!("/api/flow?from=sym:src/main.rs%23cmd_index&{q}"),
        )
        .await;
        assert_eq!(status, 200);
        let run_all = f["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == "run_all")
            .unwrap()
            .clone();
        assert_eq!(run_all["more"], 0, "{q}");
    }
    // Depth and budget.
    let (_, f) = get(&app, "/api/flow?from=cmd_index&depth=1").await;
    assert_eq!(f["nodes"].as_array().unwrap().len(), 4);
    let (_, f) = get(&app, "/api/flow?from=cmd_index&max=10").await;
    assert_eq!(f["nodes"].as_array().unwrap().len(), 10);
}

#[tokio::test]
async fn flow_errors_are_data() {
    let app = api::router(flow_state());
    let (status, e) = get(&app, "/api/flow").await;
    assert_eq!(
        (status, e["error"]["code"].as_str()),
        (400, Some("bad_request"))
    );
    let (status, e) = get(&app, "/api/flow?from=sym:nope.rs%23x").await;
    assert_eq!(
        (status, e["error"]["code"].as_str()),
        (404, Some("not_found"))
    );
    let (status, _) = get(&app, "/api/flow?from=cmd_index&depth=-1").await;
    assert_eq!(status, 400);
    let (status, _) = get(&app, "/api/symbol?id=sym:nope.rs%23x").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn symbol_lists_production_callers_first_and_counts_tests() {
    let app = api::router(flow_state());
    let (status, s) = get(&app, "/api/symbol?id=sym:src/steps.rs%23a1").await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["indexed"], true);
    assert_eq!(s["symbol"]["name"], "a1");
    assert_eq!(s["symbol"]["kind"], "function");
    assert_eq!(s["symbol"]["exported"], true);
    assert_eq!(
        names(&s["callers"]["production"], "file"),
        vec!["src/steps.rs", "src/store.rs"]
    );
    assert_eq!(s["callers"]["production"][0]["caller_name"], "run_all");
    assert_eq!(s["callers"]["test_sites"], 1);
    assert_eq!(names(&s["callees"], "name"), vec!["flush"]);
    assert_eq!(s["finding"], "warning");
    assert_eq!(s["git"], Value::Null);

    // A flow rooted at a file (a script, a scene) has a card too.
    let (status, f) = get(&app, "/api/symbol?id=file:src/store.rs").await;
    assert_eq!(status, 200, "{f}");
    assert_eq!(f["symbol"]["kind"], "file");
    assert_eq!(f["symbol"]["name"], "store.rs");
    let (status, _) = get(&app, "/api/symbol?id=file:nope.rs").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn file_flows_and_function_search() {
    let app = api::router(flow_state());
    let (status, f) = get(&app, "/api/file-flows?id=file:src/store.rs").await;
    assert_eq!(status, 200, "{f}");
    assert_eq!(names(&f["functions"], "name"), vec!["flush", "save"]);
    assert!(names(&f["entries"], "name").contains(&"index".to_string()));
    let (status, _) = get(&app, "/api/file-flows?id=file:nope.rs").await;
    assert_eq!(status, 404);

    let (status, r) = get(&app, "/api/search?q=chain&kinds=sym").await;
    assert_eq!(status, 200);
    let hits = r["results"].as_array().unwrap();
    assert_eq!(hits.len(), 5);
    assert!(hits.iter().all(|h| h["kind"] == "sym"));
    assert_eq!(hits[0]["id"], "sym:src/steps.rs#chain1");
    assert_eq!(hits[0]["path"], "src/steps.rs");
    // The default search is unchanged: no functions.
    let (_, r) = get(&app, "/api/search?q=steps").await;
    assert!(
        r["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["kind"] != "sym")
    );
    let (_, r) = get(&app, "/api/search?q=steps&kinds=file,sym").await;
    assert!(
        r["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["kind"] == "file")
    );
    let (status, _) = get(&app, "/api/search?q=x&kinds=bogus").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn an_index_without_symbols_answers_indexed_false() {
    // The plain test snapshot has no symbol layer, as an index written before it.
    let app = api::router(ready_state("imports-ts"));
    for uri in [
        "/api/entries",
        "/api/flow?from=main",
        "/api/symbol?id=main",
        "/api/file-flows?id=file:x",
    ] {
        let (status, b) = get(&app, uri).await;
        assert_eq!(status, 200, "{uri}: {b}");
        assert_eq!(b["indexed"], false, "{uri}");
        assert_eq!(b["generation"], 1);
    }
    let (_, e) = get(&app, "/api/entries").await;
    assert_eq!(e["entries"], serde_json::json!([]));
}

/// The real server reads the symbol layer from the store, and a re-index serves the flow
/// again under the new generation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_serves_flows_from_the_store_across_reindexes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("flow-rust");
    copy_dir(&fixture("flow-rust"), &root);
    let server = start(ServeConfig {
        port: Some(0),
        watch: false,
        git: false,
        ..ServeConfig::new(&root)
    })
    .await
    .unwrap();
    let addr = server.addr;
    let g = settle(addr, Duration::from_millis(800)).await;
    let (status, e) = http_json(addr, "/api/entries").await;
    assert_eq!(status, 200);
    assert_eq!(e["indexed"], true, "{e}");
    assert_eq!(e["entries"].as_array().unwrap().len(), 4);

    // A new function called from cmd_serve.
    std::fs::write(
        root.join("src/routes.rs"),
        "use axum::routing::get;\nuse axum::Router;\n\npub fn router() -> Router {\n    warm();\n    Router::new().route(\"/health\", get(health))\n}\n\nfn warm() {}\n\nasync fn health() -> String {\n    String::new()\n}\n",
    )
    .unwrap();
    let mut events = sse(addr).await;
    let _ = http(addr, "POST", "/api/reindex").await;
    next_event(&mut events, |e| {
        e.event == "index-finished" && e.data["generation"].as_u64().unwrap_or(0) > g
    })
    .await;
    let (status, f) = http_json(addr, "/api/flow?from=sym:src/main.rs%23cmd_serve").await;
    assert_eq!(status, 200, "{f}");
    assert!(f["generation"].as_u64().unwrap() > g);
    let ids: Vec<&str> = f["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"sym:src/routes.rs#warm"), "{ids:?}");
    server.stop().await;
}
