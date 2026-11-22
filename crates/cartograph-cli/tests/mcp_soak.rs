//! Soak: hundreds of mixed requests (valid, bad arguments, unknown tools, garbage lines),
//! many in flight at once, while files change under the watcher. The server must answer
//! every request exactly once, never write anything but JSON-RPC to stdout, stay up, and
//! keep its memory bounded.

mod mcp_client;

use mcp_client::{Client, fixture, rss_kib};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Deterministic pseudo-random numbers (no extra dependency).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
}

fn request(rng: &mut Rng) -> (String, Value) {
    let nodes = [
        "src/main.ts",
        "src/core/model.ts",
        "src/core",
        "dir:src/ui",
        "file:src/core/service.ts",
        "src/nope.ts",
        "",
        "../../etc/passwd",
        "C:\\Windows\\system32",
        "💥",
    ];
    let node = rng.pick(&nodes).to_string();
    let node2 = rng.pick(&nodes).to_string();
    let (tool, args) = match rng.next() % 16 {
        0 => ("overview", json!({"path": node, "depth": rng.next() % 6})),
        1 => ("explain", json!({"node": node})),
        2 => (
            "deps",
            json!({"node": node, "dir": *rng.pick(&["in", "out", "both", "up"]), "depth": rng.next() % 5}),
        ),
        3 => ("impact", json!({"nodes": [node, node2]})),
        4 => (
            "find",
            json!({"query": *rng.pick(&["service", "mod", "", "zzz", "Model", "a/b"]), "kind": *rng.pick(&["file", "dir", "sym", "ext", "x"])}),
        ),
        5 => ("path", json!({"from": node, "to": node2})),
        6 => ("symbols", json!({"node": node})),
        7 => (
            *rng.pick(&["callers", "hierarchy"]),
            json!({"symbol": *rng.pick(&["Service", "src/core/model.ts#Model", "nope", "sym:src/core/service.ts#Service.run", ""])}),
        ),
        8 => (
            "entries",
            json!({"kind": *rng.pick(&["main", "http", "nah"])}),
        ),
        9 => (
            "findings",
            json!({"kind": [*rng.pick(&["cycle", "hub", "bogus"])], "severity": *rng.pick(&["info", "error", "loud"])}),
        ),
        10 => ("check", json!({})),
        11 => ("annotate", json!({"node": node, "text": "soak note"})),
        // Bad shapes: wrong types, missing fields, unknown tools.
        12 => ("explain", json!({"node": 42})),
        13 => ("impact", json!({"nodes": "src/main.ts"})),
        14 => ("deps", json!({})),
        _ => ("no_such_tool", json!({})),
    };
    (tool.to_string(), args)
}

#[test]
fn soak_mixed_concurrent_requests() {
    let (_tmp, root, data) = fixture("query-app");
    let d = data.display().to_string();
    let mut c = Client::spawn(&root, &["--data-dir", &d]);
    c.handshake();
    let pid = c.child.id();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let t0 = Instant::now();
    let mut answered = 0usize;
    let mut errors: HashMap<&'static str, usize> = HashMap::new();
    let mut rss_after_warmup = None;
    const ROUNDS: usize = 12;
    const BATCH: usize = 50;
    for round in 0..ROUNDS {
        // Garbage between requests must be ignored.
        c.send_raw("not json at all\n");
        c.send_raw("{\"jsonrpc\":\"2.0\",\"id\":\n");
        // A file changes under the watcher every few rounds.
        if round % 3 == 1 {
            let f = root.join("src/core/util.ts");
            let body = std::fs::read_to_string(&f).unwrap();
            std::fs::write(&f, format!("{body}\nexport const r{round} = {round};\n")).unwrap();
        }
        let ids: Vec<u64> = (0..BATCH)
            .map(|_| {
                let (tool, args) = request(&mut rng);
                c.request_async("tools/call", json!({"name": tool, "arguments": args}))
            })
            .collect();
        for id in ids {
            let r = c.wait_id(id, Duration::from_secs(120));
            answered += 1;
            let kind = if r.get("error").is_some() {
                "protocol_error"
            } else if r["result"]["isError"] == true {
                "tool_error"
            } else {
                "ok"
            };
            *errors.entry(kind).or_default() += 1;
            if let Some(t) = r["result"]["content"][0]["text"].as_str() {
                assert!(!t.contains("panic"), "panic surfaced: {t}");
                assert!(t.len() <= 1600 * 4, "over budget: {} chars", t.len());
            }
        }
        if round == 2 {
            rss_after_warmup = rss_kib(pid);
        }
    }
    let elapsed = t0.elapsed();
    assert_eq!(answered, ROUNDS * BATCH);
    assert!(errors.get("ok").copied().unwrap_or(0) > 100, "{errors:?}");
    // No response arrived twice or unasked (the client keeps unmatched ones).
    assert!(c.read(Duration::from_millis(300)).is_none());
    let rss_end = rss_kib(pid);
    eprintln!(
        "soak: {answered} requests in {elapsed:?} ({:?} per request), outcomes {errors:?}, \
         rss after warm-up {rss_after_warmup:?} KiB, at end {rss_end:?} KiB",
        elapsed / answered as u32
    );
    if let (Some(a), Some(b)) = (rss_after_warmup, rss_end) {
        assert!(b < a * 2 + 64 * 1024, "memory grew from {a} to {b} KiB");
    }
    // Still healthy.
    let r = c.call("overview", json!({}), Duration::from_secs(60));
    assert_eq!(r["isError"], false, "{r}");
    let st = c.shutdown(Duration::from_secs(10));
    assert!(st.success(), "{st:?}");
}
