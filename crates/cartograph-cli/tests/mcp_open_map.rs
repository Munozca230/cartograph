//! `open_map` over a real stdio session: the web map starts inside the MCP server on a free
//! port, serves the repo the server indexes, is reused by later calls, follows the engine's
//! re-indexes, refuses foreign hosts, and goes away when stdin closes.

mod mcp_client;

use mcp_client::{Client, fixture, text};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(60);

fn spawn(root: &std::path::Path, data: &std::path::Path) -> Client {
    let d = data.display().to_string();
    Client::spawn(root, &["--data-dir", &d])
}

/// Call until the answer is not "indexing" (the first index may still run).
fn call_ready(c: &mut Client, args: Value) -> Value {
    let deadline = Instant::now() + T;
    loop {
        let r = c.call("open_map", args.clone(), T);
        if !text(&r).contains("is indexing") || Instant::now() > deadline {
            return r;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The URL in an `open_map` answer.
fn url_of(r: &Value) -> String {
    let t = text(r);
    let start = t
        .find("http://127.0.0.1:")
        .unwrap_or_else(|| panic!("no URL in {t}"));
    t[start..].split_whitespace().next().unwrap().to_string()
}

/// `127.0.0.1:<port>` and the path of a URL.
fn split(url: &str) -> (String, String) {
    let rest = url.strip_prefix("http://").unwrap();
    let (addr, path) = rest.split_once('/').unwrap();
    (addr.to_string(), format!("/{path}"))
}

/// A plain HTTP/1.1 request: (status, body).
fn http(method: &str, addr: &str, path: &str, host: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).to_string();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("not an HTTP answer: {raw}"));
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

fn get(addr: &str, path: &str) -> (u16, String) {
    http("GET", addr, path, addr)
}

/// `generation` of `/api/meta`.
fn generation(addr: &str) -> u64 {
    let (status, body) = get(addr, "/api/meta");
    assert_eq!(status, 200, "{body}");
    let start = body.find('{').unwrap();
    let meta: Value = serde_json::from_str(&body[start..body.rfind('}').unwrap() + 1]).unwrap();
    meta["generation"].as_u64().unwrap()
}

/// Wait until `/api/meta` reports a generation above `than`.
fn wait_newer(addr: &str, than: u64) -> u64 {
    let deadline = Instant::now() + T;
    loop {
        let g = generation(addr);
        if g > than {
            return g;
        }
        assert!(
            Instant::now() < deadline,
            "the map stayed at generation {g}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn open_map_serves_the_indexed_repo_and_stops_with_the_session() {
    let (_tmp, root, data) = fixture("query-app");
    let mut c = spawn(&root, &data);
    c.handshake();

    // A folder: the plan, focused on it.
    let r = call_ready(&mut c, json!({"focus": "src/core", "open_browser": false}));
    assert_eq!(r["isError"], false, "{r}");
    let url = url_of(&r);
    assert!(url.ends_with("/?view=plan&focus=dir:src/core"), "{url}");
    assert!(text(&r).contains("folder src/core/"), "{}", text(&r));
    let (addr, path) = split(&url);
    let port: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();
    assert!(port != 7878 && port != 7879, "{port}");

    // The page and the API of this repo.
    let (status, page) = get(&addr, &path);
    assert_eq!(status, 200);
    assert!(page.contains("<html"), "{page}");
    let (status, meta) = get(&addr, "/api/meta");
    assert_eq!(status, 200, "{meta}");
    assert!(meta.contains("\"name\":\"query-app\""), "{meta}");
    assert!(meta.contains("\"status\":\"ready\""), "{meta}");
    let (status, tree) = get(&addr, "/api/tree");
    assert_eq!(status, 200, "{tree}");
    assert!(tree.contains("src/core/service.ts"), "{tree}");

    // DNS rebinding: another name for 127.0.0.1 is refused, the loopback names are not.
    let (status, body) = http("GET", &addr, "/api/meta", &format!("evil.example:{port}"));
    assert_eq!(status, 403, "{body}");
    let (status, _) = http("GET", &addr, "/", "evil.example");
    assert_eq!(status, 403);
    let (status, _) = http("GET", &addr, "/api/meta", &format!("localhost:{port}"));
    assert_eq!(status, 200);

    // A function: the flows view, on the same server.
    let r = c.call(
        "open_map",
        json!({"focus": "src/core/service.ts#run", "open_browser": false}),
        T,
    );
    assert_eq!(r["isError"], false, "{r}");
    let url2 = url_of(&r);
    assert_eq!(split(&url2).0, addr, "a second server was started: {url2}");
    assert!(
        url2.ends_with("/?view=flows&focus=sym:src/core/service.ts%23run"),
        "{url2}"
    );
    assert!(text(&r).contains("call flow from run"), "{}", text(&r));

    // Unknown ids and bad views are tool errors with suggestions, as in the other tools.
    let r = c.call(
        "open_map",
        json!({"focus": "src/core/servce.ts", "open_browser": false}),
        T,
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("did you mean"), "{}", text(&r));
    assert!(text(&r).contains("src/core/service.ts"), "{}", text(&r));
    let r = c.call(
        "open_map",
        json!({"focus": "src/core", "view": "flows", "open_browser": false}),
        T,
    );
    assert_eq!(r["isError"], true, "{r}");
    let r = c.call(
        "open_map",
        json!({"view": "graph", "open_browser": false}),
        T,
    );
    assert_eq!(r["isError"], true, "{r}");

    // The engine's re-index reaches the map: a new file shows up without another indexer.
    let g0 = generation(&addr);
    std::fs::write(root.join("src/core/extra.ts"), "export const extra = 1;\n").unwrap();
    let g1 = wait_newer(&addr, g0);
    let deadline = Instant::now() + T;
    loop {
        let (_, tree) = get(&addr, "/api/tree");
        if tree.contains("src/core/extra.ts") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the map never showed the new file"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // The page's reindex button asks the engine for a run.
    let (status, body) = http("POST", &addr, "/api/reindex", &addr);
    assert!(status == 200 || status == 202, "{status} {body}");
    wait_newer(&addr, g1);

    let st = c.shutdown(Duration::from_secs(10));
    assert!(st.success(), "{st:?}");
    assert!(
        TcpStream::connect(&addr).is_err(),
        "the map still listens on {addr} after the session ended"
    );
}
