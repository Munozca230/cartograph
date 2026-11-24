//! End-to-end tests against a real server on a socket: the watcher (modify, create,
//! delete, rename), no indexing loop, single-flight indexing, the 503 window, SSE and ports.

mod common;

use cartograph_server::indexer::{IndexFn, IndexRequest, default_index_fn};
use cartograph_server::{ServeConfig, ServeError, bind, start, start_with};
use common::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn config(root: &Path) -> ServeConfig {
    ServeConfig {
        port: Some(0),
        ..ServeConfig::new(root)
    }
}

fn temp_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    copy_dir(&fixture(name), &root);
    (tmp, root)
}

async fn node_status(addr: std::net::SocketAddr, id: &str) -> (u16, serde_json::Value) {
    http_json(addr, &format!("/api/node?id={id}")).await
}

/// Wait until the API shows `pred`, then check that the generation moved past `before`
/// and that an `index-finished` event for (at least) that generation arrived.
async fn expect_change<F, Fut>(
    addr: std::net::SocketAddr,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<Sse>,
    before: u64,
    what: &str,
    pred: F,
) -> u64
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<()>>,
{
    wait_for(what, pred).await;
    let g = generation(addr).await;
    assert!(
        g > before,
        "{what}: generation {g} did not move past {before}"
    );
    let ev = next_event(events, |e| {
        e.event == "index-finished" && e.data["generation"].as_u64().unwrap_or(0) >= g
    })
    .await;
    assert!(ev.data["counts"]["files"].as_u64().is_some());
    g
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watcher_reflects_modify_create_delete_and_rename() {
    let (_tmp, root) = temp_fixture("imports-ts");
    let server = start(config(&root)).await.unwrap();
    let addr = server.addr;
    let mut events = sse(addr).await;
    let hello = next_event(&mut events, |e| e.event == "hello").await;
    assert!(hello.data["generation"].is_u64());
    let (_, meta) = http_json(addr, "/api/meta").await;
    assert_eq!(meta["watching"], true);
    let mut g = settle(addr, Duration::from_millis(1500)).await;
    assert!(g >= 1);

    // Modify: config/app.ts starts importing ../scripts/tool.
    std::fs::write(
        root.join("config/app.ts"),
        "import { port } from '../scripts/tool';\nexport const cfg = { port: 1 };\nexport const p = port;\n",
    )
    .unwrap();
    g = expect_change(addr, &mut events, g, "modified import", || async move {
        let (s, n) = node_status(addr, "file:config/app.ts").await;
        (s == 200
            && n["imports"]
                .as_array()?
                .iter()
                .any(|i| i["target"] == "file:scripts/tool.ts"))
        .then_some(())
    })
    .await;

    // Create: a new file importing helper.ts.
    std::fs::write(
        root.join("apps/web/src/extra.ts"),
        "import { helper } from './helper';\nexport const x = helper();\n",
    )
    .unwrap();
    g = expect_change(addr, &mut events, g, "created file", || async move {
        let (s, n) = node_status(addr, "file:apps/web/src/helper.ts").await;
        (s == 200
            && n["importers"]
                .as_array()?
                .iter()
                .any(|i| i["source"] == "file:apps/web/src/extra.ts"))
        .then_some(())
    })
    .await;

    // Delete it again.
    std::fs::remove_file(root.join("apps/web/src/extra.ts")).unwrap();
    g = expect_change(addr, &mut events, g, "deleted file", || async move {
        (node_status(addr, "file:apps/web/src/extra.ts").await.0 == 404).then_some(())
    })
    .await;

    // Rename helper.ts: the old id disappears, the new one appears, and App.tsx's import
    // of './helper.js' becomes unresolved.
    std::fs::rename(
        root.join("apps/web/src/helper.ts"),
        root.join("apps/web/src/helper2.ts"),
    )
    .unwrap();
    let _ = expect_change(addr, &mut events, g, "renamed file", || async move {
        let old = node_status(addr, "file:apps/web/src/helper.ts").await.0;
        let (new, _) = node_status(addr, "file:apps/web/src/helper2.ts").await;
        let (_, app) = node_status(addr, "file:apps/web/src/App.tsx").await;
        let unresolved = app["unresolved"]
            .as_array()?
            .iter()
            .any(|u| u["specifier"] == "./helper.js");
        (old == 404 && new == 200 && unresolved).then_some(())
    })
    .await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writing_the_database_inside_the_repo_does_not_retrigger_indexing() {
    for data_dir_is_root in [false, true] {
        let (_tmp, root) = temp_fixture("imports-ts");
        let cfg = ServeConfig {
            // Default: <repo>/.cartograph. Second round: the repo root itself.
            data_dir: data_dir_is_root.then(|| root.clone()),
            ..config(&root)
        };
        let server = start(cfg).await.unwrap();
        let addr = server.addr;
        let g = settle(addr, Duration::from_millis(1500)).await;
        assert!(g >= 1);
        let db = if data_dir_is_root {
            root.join("graph.db")
        } else {
            root.join(".cartograph/graph.db")
        };
        assert!(db.is_file());
        // A server-driven index writes the symbol layer too (`query callers` on a live repo).
        let symbols = cartograph_core::Store::open(&db)
            .unwrap()
            .load_symbols()
            .unwrap();
        assert!(!symbols.symbols.is_empty() && !symbols.calls.is_empty());
        let mut events = sse(addr).await;
        next_event(&mut events, |e| e.event == "hello").await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(
            generation(addr).await,
            g,
            "data dir in root: {data_dir_is_root}"
        );
        while let Ok(ev) = events.try_recv() {
            assert_ne!(ev.event, "index-started", "unexpected run: {ev:?}");
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_endpoints_are_503_until_the_first_index_then_200() {
    let (_tmp, root) = temp_fixture("imports-ts");
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let g2 = gate.clone();
    let real = default_index_fn();
    let gated: IndexFn = Arc::new(move |req: &IndexRequest| {
        let (lock, cv) = &*g2;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        real(req)
    });
    let server = start_with(
        ServeConfig {
            watch: false,
            ..config(&root)
        },
        gated,
    )
    .await
    .unwrap();
    let addr = server.addr;
    // The HTTP server answers while the index is held.
    let (status, e) = http_json(addr, "/api/graph").await;
    assert_eq!(status, 503);
    assert_eq!(e["error"]["code"], "indexing");
    let (status, m) = http_json(addr, "/api/meta").await;
    assert_eq!((status, m["status"].as_str()), (200, Some("indexing")));
    let mut events = sse(addr).await;
    let hello = next_event(&mut events, |e| e.event == "hello").await;
    assert_eq!(hello.data["status"], "indexing");
    assert_eq!(hello.data["generation"], 0);

    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    let fin = next_event(&mut events, |e| e.event == "index-finished").await;
    assert_eq!(fin.data["generation"], 1);
    let (status, g) = http_json(addr, "/api/graph").await;
    assert_eq!((status, g["generation"].as_u64()), (200, Some(1)));
    assert_eq!(http_json(addr, "/api/meta").await.1["watching"], false);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn never_two_indexes_at_once_and_one_queued_run_for_a_burst() {
    let (_tmp, root) = temp_fixture("imports-ts");
    let calls = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let overlap = Arc::new(AtomicBool::new(false));
    // The first run is held at this gate until the whole burst has been sent.
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let real = default_index_fn();
    let (c, a, o, g) = (calls.clone(), active.clone(), overlap.clone(), gate.clone());
    let slow: IndexFn = Arc::new(move |req: &IndexRequest| {
        c.fetch_add(1, Ordering::SeqCst);
        if a.fetch_add(1, Ordering::SeqCst) > 0 {
            o.store(true, Ordering::SeqCst);
        }
        {
            let (lock, cv) = &*g;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        let r = real(req);
        a.fetch_sub(1, Ordering::SeqCst);
        r
    });
    let server = start_with(
        ServeConfig {
            watch: false,
            ..config(&root)
        },
        slow,
    )
    .await
    .unwrap();
    let addr = server.addr;
    wait_for("first run to start", || {
        let c = calls.clone();
        async move { (c.load(Ordering::SeqCst) == 1).then_some(()) }
    })
    .await;
    for _ in 0..20 {
        let (status, _) = http(addr, "POST", "/api/reindex").await;
        assert_eq!(status, 202);
    }
    let mut events = sse(addr).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "still held at the gate");
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    next_event(&mut events, |e| {
        e.event == "index-finished" && e.data["generation"] == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "startup run + exactly one queued run"
    );
    assert!(!overlap.load(Ordering::SeqCst), "two indexes ran at once");
    assert_eq!(generation(addr).await, 2);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn index_errors_are_reported_and_the_server_keeps_serving() {
    let (_tmp, root) = temp_fixture("imports-ts");
    let failing: IndexFn = Arc::new(|_req: &IndexRequest| Err("disk on fire".to_string()));
    let server = start_with(
        ServeConfig {
            watch: false,
            ..config(&root)
        },
        failing,
    )
    .await
    .unwrap();
    let addr = server.addr;
    wait_for("error status", || async move {
        let (_, m) = http_json(addr, "/api/meta").await;
        (m["status"] == "error").then_some(m)
    })
    .await;
    let (status, e) = http_json(addr, "/api/tree").await;
    assert_eq!(
        (status, e["error"]["code"].as_str()),
        (500, Some("internal"))
    );
    server.stop().await;
}

#[tokio::test]
async fn an_explicit_taken_port_fails_and_the_default_one_moves_on() {
    let (_taken, port) = taken_port_with_a_free_neighbour();
    // Explicit port: a clean error.
    match bind(Some(port), 7878) {
        Err(ServeError::PortInUse { port: p, .. }) => assert_eq!(p, port),
        other => panic!("expected PortInUse, got {other:?}"),
    }
    let (_tmp, root) = temp_fixture("imports-ts");
    let err = start(ServeConfig {
        port: Some(port),
        watch: false,
        ..ServeConfig::new(&root)
    })
    .await
    .err()
    .unwrap();
    assert!(err.to_string().contains(&port.to_string()), "{err}");

    // No explicit port: the next free one within 10.
    let l = bind(None, port).unwrap();
    let got = l.local_addr().unwrap().port();
    assert!(got > port && got <= port + 10, "{got} vs {port}");
    assert!(l.local_addr().unwrap().ip().is_loopback());
}

/// A port the OS hands out, held, whose next port can be bound too. An ephemeral port can
/// sit just below a range the system reserves (Windows excludes blocks of ports), and then
/// no port after it is free: the default port would rightly find none, which is not what
/// the test above checks.
fn taken_port_with_a_free_neighbour() -> (std::net::TcpListener, u16) {
    for _ in 0..50 {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        if port <= u16::MAX - cartograph_server::PORT_TRIES
            && std::net::TcpListener::bind(("127.0.0.1", port + 1)).is_ok()
        {
            return (taken, port);
        }
    }
    panic!("no ephemeral port with a bindable next port in 50 tries");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_during_an_index_cancels_it_promptly() {
    let (_tmp, root) = temp_fixture("imports-ts");
    let started = Arc::new(AtomicBool::new(false));
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let (s, c) = (started.clone(), saw_cancel.clone());
    // An index that only ends when it is cancelled (or after a minute).
    let endless: IndexFn = Arc::new(move |req: &IndexRequest| {
        s.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if req.progress.cancelled() {
                c.store(true, Ordering::SeqCst);
                return Err("cancelled".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err("never cancelled".into())
    });
    let server = start_with(
        ServeConfig {
            watch: false,
            ..config(&root)
        },
        endless,
    )
    .await
    .unwrap();
    wait_for("index to start", || {
        let s = started.clone();
        async move { s.load(Ordering::SeqCst).then_some(()) }
    })
    .await;
    let t = std::time::Instant::now();
    server.stop().await;
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "stop took {:?}",
        t.elapsed()
    );
    wait_for("the index to see the cancellation", || {
        let c = saw_cancel.clone();
        async move { c.load(Ordering::SeqCst).then_some(()) }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn findings_follow_cartograph_toml_and_its_edits() {
    let (_tmp, root) = temp_fixture("query-app");
    let server = start(config(&root)).await.unwrap();
    let addr = server.addr;
    let g = settle(addr, Duration::from_millis(1500)).await;
    let (status, f) = http_json(addr, "/api/findings").await;
    assert_eq!(status, 200);
    assert_eq!(f["config"]["origin"], "cartograph.toml");
    assert_eq!(f["summary"]["violations"], 5, "{f}");

    // A broken file: the server keeps serving, says why, and the rules no longer apply.
    std::fs::write(root.join("cartograph.toml"), "[rules]\nmax_fan_out = 0\n").unwrap();
    wait_for("broken cartograph.toml reported", || async move {
        let (_, f) = http_json(addr, "/api/findings").await;
        (generation(addr).await > g && f["config"]["error"].is_string()).then_some(())
    })
    .await;
    let (_, f) = http_json(addr, "/api/findings").await;
    assert_eq!(f["summary"]["violations"], 0);
    let err = f["config"]["error"].as_str().unwrap();
    assert!(
        err.starts_with("cartograph.toml:2: rules.max_fan_out"),
        "{err}"
    );
    server.stop().await;
}
