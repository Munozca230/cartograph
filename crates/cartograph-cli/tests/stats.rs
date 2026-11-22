//! Usage stats: the MCP server writes one line per tool call (errors of every kind
//! included) with no argument or path in it, writing the log inside the repo does not
//! trigger a re-index, and `cartograph stats` summarizes it.

mod mcp_client;

use mcp_client::{Client, fixture, text};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(60);

fn call_ready(c: &mut Client, tool: &str, args: Value) -> Value {
    let deadline = Instant::now() + T;
    loop {
        let r = c.call(tool, args.clone(), T);
        if !text(&r).contains("is indexing") || Instant::now() > deadline {
            return r;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stats_json(root: &Path, data: Option<&Path>) -> Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cartograph"));
    cmd.arg("stats").arg(root).arg("--json");
    if let Some(d) = data {
        cmd.arg("--data-dir").arg(d);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn every_call_is_one_line_without_arguments() {
    let (_tmp, root, data) = fixture("query-app");
    let d = data.display().to_string();
    let mut c = Client::spawn(&root, &["--data-dir", &d]);
    c.handshake();
    // The first call may answer "indexing" more than once: count what was sent.
    let mut sent = 0;
    let deadline = Instant::now() + T;
    loop {
        let r = c.call("overview", json!({}), T);
        sent += 1;
        if !text(&r).contains("is indexing") || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let r = c.call("impact", json!({"nodes": ["src/core/model.ts"]}), T);
    assert_eq!(r["isError"], false, "{r}");
    // Errors keep no structuredContent without --structured.
    let r = c.call(
        "explain",
        json!({"node": "src/core/no-such-secret-file.ts"}),
        T,
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(r.get("structuredContent").is_none(), "{r}");
    let r = c.call("deps", json!({"node": "src/core/model.ts", "dir": "up"}), T);
    assert_eq!(r["isError"], true, "{r}");
    let r = c.call("callers", json!({"symbol": 42}), T);
    assert_eq!(r["isError"], true, "{r}");
    let r = c.request(
        "tools/call",
        json!({"name": "secret_tool_name", "arguments": {}}),
        T,
    );
    assert!(r.get("error").is_some(), "{r}");
    sent += 5;
    c.shutdown(Duration::from_secs(10));

    let log = std::fs::read_to_string(data.join("stats.jsonl")).unwrap();
    assert_eq!(log.lines().count(), sent, "{log}");
    for secret in ["model.ts", "no-such-secret", "src/core", "secret_tool_name"] {
        assert!(
            !log.contains(secret),
            "{secret} leaked into the stats:\n{log}"
        );
    }
    let s = stats_json(&root, Some(&data));
    assert_eq!(s["calls"], sent, "{s}");
    assert_eq!(s["sessions"], 1, "{s}");
    let errors = &s["errors"];
    assert_eq!(errors["unknown_node"], 1, "{s}");
    assert_eq!(errors["bad_argument"], 1, "{s}");
    assert_eq!(errors["bad_params"], 1, "{s}");
    assert_eq!(errors["unknown_tool"], 1, "{s}");
    let tools = s["tools"].as_array().unwrap();
    let impact = tools.iter().find(|t| t["tool"] == "impact").unwrap();
    assert_eq!(impact["calls"], 1);
    assert!(impact["avg_tokens"].as_u64().unwrap() > 0, "{impact}");
}

#[test]
fn the_log_does_not_reindex() {
    let (_tmp, root, _data) = fixture("query-app");
    // Default data dir inside the repo: the log is written where the watcher looks. An index
    // built first (and a pause for the file events of the fixture copy to pass) leaves the
    // server's startup run as the only expected one.
    let out = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .arg("index")
        .arg(&root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    std::thread::sleep(Duration::from_millis(500));
    let mut c = Client::spawn(&root, &["-v"]);
    c.handshake();
    call_ready(&mut c, "overview", json!({}));
    for _ in 0..5 {
        c.call("find", json!({"query": "model"}), T);
    }
    // Longer than the watcher's debounce: a re-index caused by the log would show by now.
    std::thread::sleep(Duration::from_millis(1500));
    c.call("find", json!({"query": "model"}), T);
    let stderr = c.stderr.lock().unwrap().clone();
    c.shutdown(Duration::from_secs(10));
    let runs = stderr.matches("index finished").count();
    assert_eq!(runs, 1, "the stats log re-indexed the repo:\n{stderr}");

    let s = stats_json(&root, None);
    let tools = s["tools"].as_array().unwrap();
    let find = tools.iter().find(|t| t["tool"] == "find").unwrap();
    assert_eq!(find["calls"], 6, "{s}");

    let out = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .arg("stats")
        .arg(&root)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("find"), "{text}");
}
