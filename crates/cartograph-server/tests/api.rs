//! One test per endpoint on fixtures (router called in-process), plus the error envelope,
//! panic isolation and the `indexing` state.

mod common;

use cartograph_server::api;
use cartograph_server::state::AppState;
use common::*;
use serde_json::Value;
use std::sync::Arc;

fn app(name: &str) -> axum::Router {
    api::router(ready_state(name))
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn meta_reports_counts_languages_and_status() {
    let (status, m) = get(&app("imports-ts"), "/api/meta").await;
    assert_eq!(status, 200);
    assert_eq!(m["generation"], 1);
    assert_eq!(m["status"], "ready");
    assert_eq!(m["progress"], Value::Null);
    assert_eq!(m["repo"]["name"], "imports-ts");
    assert!(!m["repo"]["root"].as_str().unwrap().contains('\\'));
    let c = &m["counts"];
    assert_eq!(c["files"], 34);
    assert_eq!(c["packages"], 6);
    // Every import occurrence is classified once.
    assert_eq!(
        c["imports"].as_u64(),
        Some(
            c["resolved"].as_u64().unwrap()
                + c["external"].as_u64().unwrap()
                + c["unresolved"].as_u64().unwrap()
        )
    );
    assert!(
        m["languages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["lang"] == "typescript")
    );
    assert_eq!(m["watching"], false);
}

#[tokio::test]
async fn tree_is_the_directory_hierarchy_with_metrics() {
    let app = app("imports-ts");
    let (status, t) = get(&app, "/api/tree").await;
    assert_eq!(status, 200);
    let root = &t["root"];
    assert_eq!(root["id"], "repo:");
    assert_eq!(root["kind"], "repo");
    assert_eq!(root["files"], 34);
    // Dirs first, then files, each sorted by name; top-level files hang off the root.
    let kids = ids(&root["children"]);
    assert_eq!(
        kids,
        [
            "dir:apps",
            "dir:config",
            "dir:packages",
            "dir:scripts",
            "file:README.md",
            "file:package.json",
            "file:tsconfig.base.json"
        ]
    );
    // `packages` is imported by files outside it (apps and scripts).
    let packages = &root["children"][2];
    assert!(packages["fan_in"].as_u64().unwrap() > 0);
    assert_eq!(packages["fan_out"], 0);
    // Files and LOC add up.
    let sum: u64 = root["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.get("files").and_then(Value::as_u64).unwrap_or(1))
        .sum();
    assert_eq!(sum, 34);

    let (_, t) = get(&app, "/api/tree?depth=1").await;
    let apps = &t["root"]["children"][0];
    assert_eq!(apps["truncated"], true);
    assert!(apps.get("children").is_none());

    let (status, t) = get(&app, "/api/tree?root=dir:apps/web&depth=1").await;
    assert_eq!(status, 200);
    assert_eq!(t["root"]["id"], "dir:apps/web");
    let src = &t["root"]["children"][0];
    assert_eq!(src["id"], "dir:apps/web/src");
    assert_eq!(src["truncated"], true);

    // `dir:` is the root too.
    let (status, t) = get(&app, "/api/tree?root=dir:&depth=0").await;
    assert_eq!((status, t["root"]["id"].as_str()), (200, Some("repo:")));

    assert_eq!(get(&app, "/api/tree?root=dir:nope").await.0, 404);
    let (status, e) = get(&app, "/api/tree?root=file:package.json").await;
    assert_eq!(
        (status, e["error"]["code"].as_str()),
        (400, Some("bad_request"))
    );
    let (status, e) = get(&app, "/api/tree?depth=x").await;
    assert_eq!(
        (status, e["error"]["code"].as_str()),
        (400, Some("bad_request"))
    );
}

#[tokio::test]
async fn graph_starts_at_the_top_level_and_expands() {
    let app = app("imports-ts");
    let (status, g) = get(&app, "/api/graph").await;
    assert_eq!(status, 200);
    assert_eq!(g["generation"], 1);
    let nodes = ids(&g["nodes"]);
    assert!(nodes.contains(&"dir:apps".to_string()));
    assert!(nodes.contains(&"file:package.json".to_string()));
    let edge = g["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["src"] == "dir:apps" && e["dst"] == "dir:packages")
        .unwrap();
    let c = &edge["confidence"];
    assert_eq!(
        edge["weight"].as_u64(),
        Some(
            c["exact"].as_u64().unwrap()
                + c["resolved"].as_u64().unwrap()
                + c["heuristic"].as_u64().unwrap()
        )
    );
    assert!(
        g["externals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["ext"] == "ext:npm:react")
    );

    let (_, g) = get(&app, "/api/graph?expanded=dir:apps,dir:apps/web").await;
    let nodes = ids(&g["nodes"]);
    assert!(nodes.contains(&"dir:apps/web/src".to_string()));
    assert!(!nodes.contains(&"dir:apps".to_string()));
    let src = g["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "dir:apps/web/src")
        .unwrap();
    assert_eq!(src["parent"], "dir:apps/web");
    assert_eq!(src["expandable"], true);

    // A child whose parent is not expanded is ignored, and so is an unknown id.
    let (_, g) = get(&app, "/api/graph?expanded=dir:apps/web,dir:nope").await;
    assert_eq!(
        g["ignored"],
        serde_json::json!(["dir:nope", "dir:apps/web"])
    );
    assert!(ids(&g["nodes"]).contains(&"dir:apps".to_string()));
}

#[tokio::test]
async fn node_details_for_file_dir_ext_and_package() {
    let app = app("imports-ts");
    let (status, n) = get(&app, "/api/node?id=file:apps/web/src/main.tsx").await;
    assert_eq!(status, 200);
    assert_eq!(n["node"]["id"], "file:apps/web/src/main.tsx");
    assert_eq!(n["package"], "pkg:apps/web/package.json");
    let imports = n["imports"].as_array().unwrap();
    assert!(
        imports
            .iter()
            .any(|i| i["target"] == "file:apps/web/src/App.tsx"
                && i["confidence"] == "exact"
                && i["line"] == 2)
    );
    assert!(
        n["externals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["ext"] == "ext:npm:react")
    );
    assert!(
        n["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["reason"] == "ambiguous")
    );

    let (_, n) = get(&app, "/api/node?id=file:apps/web/src/helper.ts").await;
    assert!(
        n["importers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["source"] == "file:apps/web/src/App.tsx")
    );

    let (status, n) = get(&app, "/api/node?id=dir:apps/web/src").await;
    assert_eq!(status, 200);
    assert!(n["children"].as_array().unwrap().len() <= 10);
    assert_eq!(n["deps_out"][0]["id"], "dir:packages");
    assert!(
        n["externals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["ext"] == "ext:npm:react")
    );

    let (_, n) = get(&app, "/api/node?id=dir:packages").await;
    assert!(
        n["deps_in"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == "dir:apps")
    );

    let (status, n) = get(&app, "/api/node?id=repo:").await;
    assert_eq!((status, n["node"]["kind"].as_str()), (200, Some("repo")));

    // ext: and the store's external: both work.
    for id in ["ext:npm:react", "external:npm:react"] {
        let (status, n) = get(&app, &format!("/api/node?id={id}")).await;
        assert_eq!(status, 200, "{id}");
        assert_eq!(n["node"]["id"], "ext:npm:react");
        assert!(!n["importers"].as_array().unwrap().is_empty());
    }

    let (status, n) = get(&app, "/api/node?id=pkg:packages/ui/package.json").await;
    assert_eq!((status, n["node"]["name"].as_str()), (200, Some("@fx/ui")));

    assert_eq!(get(&app, "/api/node?id=file:nope.ts").await.0, 404);
    assert_eq!(get(&app, "/api/node").await.0, 400);
}

#[tokio::test]
async fn packages_are_an_overlay_with_api_ids() {
    let (status, p) = get(&app("imports-ts"), "/api/packages").await;
    assert_eq!(status, 200);
    assert_eq!(p["generation"], 1);
    let list = p["packages"].as_array().unwrap();
    assert_eq!(list.len(), 6);
    let root = list.iter().find(|x| x["id"] == "pkg:package.json").unwrap();
    assert_eq!(root["dir"], "repo:");
    assert_eq!(root["kind"], "npm");
    assert_eq!(root["manifest"], "file:package.json");
    let ui = list.iter().find(|x| x["name"] == "@fx/ui").unwrap();
    assert_eq!(ui["dir"], "dir:packages/ui");
}

#[tokio::test]
async fn issues_list_unresolved_skipped_and_diagnostics_with_a_limit() {
    let app = app("imports-ts");
    let (status, i) = get(&app, "/api/issues").await;
    assert_eq!(status, 200);
    let total = i["total"]["unresolved"].as_u64().unwrap();
    assert_eq!(i["unresolved"].as_array().unwrap().len() as u64, total);
    let by_reason: u64 = i["by_reason"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(by_reason, total);
    assert_eq!(i["truncated"], false);
    assert!(!i["diagnostics"].as_array().unwrap().is_empty());
    assert!(i["skipped"].is_array());

    let (_, i) = get(&app, "/api/issues?limit=2").await;
    assert_eq!(i["unresolved"].as_array().unwrap().len(), 2);
    assert_eq!(i["truncated"], true);
    assert_eq!(i["total"]["unresolved"].as_u64(), Some(total));
}

#[tokio::test]
async fn search_is_fuzzy_over_paths_and_externals() {
    let app = app("imports-ts");
    let (status, s) = get(&app, "/api/search?q=btn").await;
    assert_eq!(status, 200);
    assert_eq!(s["results"][0]["id"], "file:packages/ui/src/Button.tsx");
    let (_, s) = get(&app, "/api/search?q=react").await;
    assert!(
        s["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "ext:npm:react" && r["kind"] == "ext")
    );
    let (_, s) = get(&app, "/api/search?q=shared&limit=1").await;
    assert_eq!(s["results"].as_array().unwrap().len(), 1);
    assert_eq!(s["results"][0]["id"], "dir:packages/shared");
    let (_, s) = get(&app, "/api/search?q=").await;
    assert_eq!(s["results"], serde_json::json!([]));
}

#[tokio::test]
async fn reindex_is_accepted_and_queued() {
    let state = ready_state("imports-ts");
    let app = api::router(state.clone());
    let (status, r) = call(&app, "POST", "/api/reindex").await;
    assert_eq!(status, 202);
    assert_eq!(r["accepted"], true);
    assert!(state.scheduler.has_pending());
    let (status, e) = call(&app, "GET", "/api/reindex").await;
    assert_eq!((status, e["generation"].as_u64()), (405, Some(1)));
}

#[tokio::test]
async fn events_start_with_hello() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let state = ready_state("imports-ts");
    let app = api::router(state);
    let req = axum::http::Request::get("/api/events")
        .header("host", format!("127.0.0.1:{PORT}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let mut body = res.into_body();
    let frame = body.frame().await.unwrap().unwrap();
    let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(text.starts_with("event: hello\n"), "{text}");
    assert!(
        text.contains(r#""generation":1"#) && text.contains(r#""status":"ready""#),
        "{text}"
    );
}

#[tokio::test]
async fn data_endpoints_answer_503_indexing_until_the_first_snapshot() {
    let root = fixture("imports-ts");
    let state = Arc::new(AppState::new("x".into(), root.clone(), root.join(".x")));
    state.set_port(PORT);
    state.set_running();
    state.progress.start(10);
    state.progress.advance();
    let app = api::router(state.clone());
    for path in [
        "/api/tree",
        "/api/graph",
        "/api/node?id=repo:",
        "/api/packages",
        "/api/issues",
        "/api/findings",
        "/api/search?q=a",
    ] {
        let (status, e) = get(&app, path).await;
        assert_eq!(status, 503, "{path}");
        assert_eq!(e["error"]["code"], "indexing", "{path}");
        assert_eq!(
            e["progress"],
            serde_json::json!({"done": 1, "total": 10}),
            "{path}"
        );
        assert_eq!(e["generation"], 0);
    }
    let (status, m) = get(&app, "/api/meta").await;
    assert_eq!((status, m["status"].as_str()), (200, Some("indexing")));
    assert_eq!(m["progress"]["total"], 10);
    assert_eq!(m["counts"], Value::Null);

    // A failed first index is an error, not "still indexing".
    state.finish(Some("boom".into()));
    let (status, e) = get(&app, "/api/tree").await;
    assert_eq!(
        (status, e["error"]["code"].as_str()),
        (500, Some("internal"))
    );
    let (_, m) = get(&app, "/api/meta").await;
    assert_eq!(
        (m["status"].as_str(), m["error"].as_str()),
        (Some("error"), Some("boom"))
    );
}

#[tokio::test]
async fn unknown_api_paths_are_json_404_and_others_get_the_frontend() {
    let app = app("imports-ts");
    for path in ["/api/nope", "/api", "/api/meta/extra"] {
        let (status, e) = get(&app, path).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(e["error"]["code"], "not_found", "{path}");
        assert_eq!(e["generation"], 1, "{path}");
    }
    let (status, page) = get(&app, "/some/spa/route").await;
    assert_eq!(status, 200);
    let page = page.as_str().unwrap().to_string();
    if cartograph_server::assets::embedded() {
        assert!(page.to_lowercase().contains("<html"));
    } else {
        assert!(page.contains("npm --prefix web run build"), "{page}");
    }
    assert_eq!(get(&app, "/").await.0, 200);
}

#[tokio::test]
async fn a_panicking_handler_returns_500_and_the_server_keeps_serving() {
    async fn boom() -> &'static str {
        panic!("kaboom")
    }
    async fn fine() -> &'static str {
        "ok"
    }
    let app = api::with_isolation(
        axum::Router::new()
            .route("/boom", axum::routing::get(boom))
            .route("/fine", axum::routing::get(fine)),
        None,
    );
    let (status, e) = get(&app, "/boom").await;
    assert_eq!(status, 500);
    assert_eq!(e["error"]["code"], "internal");
    assert!(e["error"]["message"].as_str().unwrap().contains("kaboom"));
    assert_eq!(get(&app, "/fine").await, (200, Value::String("ok".into())));
}

#[tokio::test]
async fn only_loopback_hosts_are_served_and_writes_need_a_same_origin() {
    let app = app("imports-ts");
    for host in [
        "127.0.0.1:7878",
        "localhost:7878",
        "LOCALHOST:7878",
        "[::1]:7878",
    ] {
        let (status, _) = call_with(&app, "GET", "/api/meta", &[("host", host)]).await;
        assert_eq!(status, 200, "{host}");
    }
    // DNS rebinding: another name, another port, or no Host at all.
    for headers in [
        vec![("host", "evil.example:7878")],
        vec![("host", "127.0.0.1:9999")],
        vec![("host", "127.0.0.1")],
        vec![("host", "127.0.0.1.evil.example:7878")],
        vec![],
    ] {
        for path in ["/api/tree", "/api/node?id=repo:", "/", "/api/events"] {
            let (status, e) = call_with(&app, "GET", path, &headers).await;
            assert_eq!(status, 403, "{headers:?} {path}");
            assert_eq!(e["error"]["code"], "forbidden", "{headers:?} {path}");
            assert_eq!(e["generation"], 1);
        }
    }
    // Cross-site writes are refused; same-origin and origin-less ones (curl) are fine.
    let host = ("host", "127.0.0.1:7878");
    for origin in [
        "http://evil.example",
        "null",
        "http://127.0.0.1:9999",
        "https://localhost:7878",
    ] {
        let (status, e) =
            call_with(&app, "POST", "/api/reindex", &[host, ("origin", origin)]).await;
        assert_eq!(
            (status, e["error"]["code"].as_str()),
            (403, Some("forbidden")),
            "{origin}"
        );
    }
    for origin in [
        "http://127.0.0.1:7878",
        "http://localhost:7878",
        "http://[::1]:7878",
    ] {
        let (status, _) =
            call_with(&app, "POST", "/api/reindex", &[host, ("origin", origin)]).await;
        assert_eq!(status, 202, "{origin}");
    }
    assert_eq!(
        call_with(&app, "POST", "/api/reindex", &[host]).await.0,
        202
    );
}

#[tokio::test]
async fn missing_files_are_404_while_routes_get_the_spa() {
    let app = app("imports-ts");
    for path in [
        "/assets/nope-123.js",
        "/assets/x",
        "/favicon.ico",
        "/deep/logo.png",
    ] {
        assert_eq!(get(&app, path).await.0, 404, "{path}");
    }
    for path in ["/", "/map", "/map/dir/src"] {
        assert_eq!(get(&app, path).await.0, 200, "{path}");
    }
}
