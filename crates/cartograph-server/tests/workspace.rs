//! `serve` over a workspace (`tests/fixtures/workspace-basic`, members made git
//! repositories at test time): `/api/meta` lists the members, ids accept the alias
//! `member:path`, and the watcher reports a change in a member's folder (named otherwise)
//! under the member's name, then re-indexes.

mod common;
#[path = "../../cartograph-index/tests/support/git_fixture.rs"]
mod git_fixture;
#[path = "../../cartograph-index/tests/support/workspace_fixture.rs"]
mod workspace_fixture;

use cartograph_server::{ServeConfig, start};
use common::*;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use workspace_fixture::workspace_basic;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_is_served_member_by_member() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let server = start(ServeConfig {
        port: Some(0),
        data_dir: Some(data.path().to_path_buf()),
        ..ServeConfig::new(&ws.root)
    })
    .await
    .unwrap();
    let addr = server.addr;
    let g = settle(addr, Duration::from_millis(1500)).await;
    assert!(g >= 1);

    let (_, meta) = http_json(addr, "/api/meta").await;
    let members = meta["workspace"]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{meta}"));
    let names: Vec<&str> = members.iter().filter_map(|m| m["name"].as_str()).collect();
    assert_eq!(names, vec!["lib", "api", "desktop", "mcp"], "{meta}");
    let api = &members[1];
    assert_eq!(api["path"], "backend-api");
    assert_eq!(api["git"], "ok", "{meta}");
    assert_eq!(api["branch"], "main");

    // The alias names the canonical node.
    let (status, node) = http_json(
        addr,
        "/api/node?id=file:api:src/main/java/com/acme/api/UserService.java",
    )
    .await;
    assert_eq!(status, 200, "{node}");
    let (status, tree) = http_json(addr, "/api/tree?root=dir:api:src&depth=1").await;
    assert_eq!(status, 200, "{tree}");

    // A change in the member's folder is re-indexed.
    std::fs::write(
        ws.root.join("backend-api/src/main/java/com/acme/api/Extra.java"),
        "package com.acme.api;\n\nimport com.acme.lib.Money;\n\npublic class Extra {\n    Money m;\n}\n",
    )
    .unwrap();
    wait_for("the new member file", || async move {
        let (s, _) = http_json(
            addr,
            "/api/node?id=file:api/src/main/java/com/acme/api/Extra.java",
        )
        .await;
        (s == 200).then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_repo_has_no_workspace_in_meta() {
    let (_, meta) = call(
        &cartograph_server::api::router(ready_state("imports-ts")),
        "GET",
        "/api/meta",
    )
    .await;
    assert!(meta.get("workspace").is_none(), "{meta}");
}

#[test]
fn the_watcher_reports_member_paths() {
    let ws = workspace_basic();
    let data = tempfile::tempdir().unwrap();
    let root = cartograph_server::normalize(&ws.root).unwrap();
    let seen: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    let sink = seen.clone();
    let _w = cartograph_server::watch::start_with(
        &root,
        data.path(),
        move |changed| sink.lock().unwrap().extend(changed),
        || {},
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let want = "desktop/src/main/users.ts";
    let deadline = Instant::now() + WAIT;
    let mut n = 0;
    while !seen.lock().unwrap().contains(want) {
        assert!(Instant::now() < deadline, "saw {:?}", seen.lock().unwrap());
        n += 1;
        std::fs::write(
            root.join("desktop-app/src/main/users.ts"),
            format!("export const touched = {n};\n"),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(500));
    }
    // Nothing outside the members is reported.
    std::fs::write(root.join("logs/app.log"), "more\n").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        seen.lock().unwrap().iter().all(|p| !p.starts_with("logs")),
        "{:?}",
        seen.lock().unwrap()
    );
}
