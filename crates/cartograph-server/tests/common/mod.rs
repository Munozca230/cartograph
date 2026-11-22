//! Shared test helpers: fixtures, snapshots, a tiny HTTP/1.0 client and an SSE reader.
#![allow(dead_code)]

use cartograph_index::IndexOptions;
use cartograph_server::snapshot::Snapshot;
use cartograph_server::state::AppState;
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Generous timeout for anything that waits on the file system (Windows can be slow).
pub const WAIT: Duration = Duration::from_secs(60);

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), &dst).unwrap();
        }
    }
}

/// Snapshot of a fixture, indexed in memory (nothing written anywhere).
pub fn snapshot_of(name: &str, generation: u64) -> Snapshot {
    let out = cartograph_index::index(&fixture(name), &IndexOptions::default()).unwrap();
    let mut meta = BTreeMap::new();
    meta.insert("repo_name".to_string(), name.to_string());
    Snapshot::build(out.graph, &meta, generation)
}

/// App state serving a fixture snapshot (no worker, no watcher).
pub fn ready_state(name: &str) -> Arc<AppState> {
    let root = fixture(name);
    let state = Arc::new(AppState::new(
        name.to_string(),
        root.clone(),
        root.join(".x"),
    ));
    state.set_port(PORT);
    state.set_snapshot(Arc::new(snapshot_of(name, 1)));
    state
}

/// Port the in-process test states pretend to listen on.
pub const PORT: u16 = 7878;

/// Call the router once; returns (status, JSON body).
pub async fn call(app: &axum::Router, method: &str, uri: &str) -> (u16, Value) {
    call_with(app, method, uri, &[("host", &format!("127.0.0.1:{PORT}"))]).await
}

/// Call the router once with exactly these headers.
pub async fn call_with(
    app: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = req.body(axum::body::Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

pub async fn get(app: &axum::Router, uri: &str) -> (u16, Value) {
    call(app, "GET", uri).await
}

/// HTTP/1.0 request over a real socket; returns (status, body text).
pub async fn http(addr: SocketAddr, method: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!("{method} {path} HTTP/1.0\r\nHost: {addr}\r\nContent-Length: 0\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}

pub async fn http_json(addr: SocketAddr, path: &str) -> (u16, Value) {
    let (status, body) = http(addr, "GET", path).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

/// One server-sent event.
#[derive(Debug, Clone)]
pub struct Sse {
    pub event: String,
    pub data: Value,
}

/// Connect to `/api/events` and forward every event to the returned channel.
pub async fn sse(addr: SocketAddr) -> mpsc::UnboundedReceiver<Sse> {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req =
        format!("GET /api/events HTTP/1.0\r\nHost: {addr}\r\nAccept: text/event-stream\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    tokio::spawn(async move {
        let mut lines = BufReader::new(s).lines();
        let (mut event, mut data) = (String::new(), String::new());
        let mut in_body = false;
        while let Ok(Some(line)) = lines.next_line().await {
            if !in_body {
                in_body = line.is_empty();
                continue;
            }
            if let Some(e) = line.strip_prefix("event:") {
                event = e.trim().to_string();
            } else if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.trim_start());
            } else if line.is_empty() && !event.is_empty() {
                let ev = Sse {
                    event: std::mem::take(&mut event),
                    data: serde_json::from_str(&std::mem::take(&mut data)).unwrap_or(Value::Null),
                };
                if tx.send(ev).is_err() {
                    break;
                }
            }
        }
    });
    rx
}

/// Wait for an event satisfying `pred`, discarding others.
pub async fn next_event(rx: &mut mpsc::UnboundedReceiver<Sse>, pred: impl Fn(&Sse) -> bool) -> Sse {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return ev,
            Ok(Some(_)) => {}
            Ok(None) => panic!("event stream closed"),
            Err(_) => panic!("timed out waiting for an event"),
        }
    }
}

/// Poll `f` every 100 ms until it returns `Some`, or panic after [`WAIT`].
pub async fn wait_for<T, F, Fut>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn generation(addr: SocketAddr) -> u64 {
    http_json(addr, "/api/meta").await.1["generation"]
        .as_u64()
        .unwrap()
}

/// Wait until the server is ready and its generation has not moved for `quiet` (a fresh
/// copy of a fixture can produce a late burst of file-system events). Returns the
/// generation. Panics if it never settles, which is what an indexing loop looks like.
pub async fn settle(addr: SocketAddr, quiet: Duration) -> u64 {
    let deadline = Instant::now() + WAIT;
    let mut last = (u64::MAX, Instant::now());
    loop {
        let (_, meta) = http_json(addr, "/api/meta").await;
        let g = meta["generation"].as_u64().unwrap_or(0);
        let busy = meta["status"] != "ready" || meta["reindexing"] == true;
        if g != last.0 || busy {
            last = (g, Instant::now());
        } else if last.1.elapsed() >= quiet {
            return g;
        }
        if Instant::now() > deadline {
            panic!("generation never settled (last {g}): indexing loop?");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
