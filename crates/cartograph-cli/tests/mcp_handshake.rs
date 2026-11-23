//! Real stdio sessions against the `cartograph mcp` binary: handshake, every tool,
//! actionable errors, start without an index, freshness after an edit, notes that persist.

mod mcp_client;

use mcp_client::{Client, fixture, text};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(60);

fn data_arg(data: &std::path::Path) -> [String; 2] {
    ["--data-dir".to_string(), data.display().to_string()]
}

fn spawn(root: &std::path::Path, data: &std::path::Path, extra: &[&str]) -> Client {
    let d = data_arg(data);
    let mut args: Vec<&str> = vec![d[0].as_str(), d[1].as_str()];
    args.extend_from_slice(extra);
    Client::spawn(root, &args)
}

/// Call until the answer is not "indexing" (the first index may still run).
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

#[test]
fn handshake_lists_and_calls_every_tool() {
    let (_tmp, root, data) = fixture("query-app");
    let mut c = spawn(&root, &data, &["--structured"]);
    let init = c.handshake();
    assert_eq!(init["serverInfo"]["name"], "cartograph");
    assert!(init["capabilities"]["tools"].is_object(), "{init}");
    let instructions = init["instructions"].as_str().unwrap();
    assert!(instructions.contains("INSTEAD"), "{instructions}");

    let list = c.request("tools/list", json!({}), T);
    let tools = list["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "annotate",
            "callers",
            "check",
            "deps",
            "diff",
            "entries",
            "explain",
            "find",
            "findings",
            "hierarchy",
            "hotspots",
            "impact",
            "open_map",
            "overview",
            "path",
            "symbols"
        ]
    );
    for t in tools {
        let d = t["description"].as_str().unwrap();
        assert!(d.len() > 80 && d.len() < 600, "{}: {d}", t["name"]);
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        let read_only = t["annotations"]["readOnlyHint"].as_bool();
        // annotate writes a note; open_map starts the web map and opens a browser.
        let writes = t["name"] == "annotate" || t["name"] == "open_map";
        assert_eq!(read_only, Some(!writes), "{t}");
    }
    // No vendor-specific metadata on any tool.
    assert!(tools.iter().all(|t| t.get("_meta").is_none()), "{tools:?}");

    let r = call_ready(&mut c, "overview", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert!(text(&r).contains("src/"), "{}", text(&r));
    assert!(r["structuredContent"].is_object(), "{r}");

    let calls = [
        ("overview", json!({"path": "src", "depth": 2})),
        ("explain", json!({"node": "src/core/model.ts"})),
        (
            "explain",
            json!({"node": root.join("src").join("core").display().to_string()}),
        ),
        (
            "deps",
            json!({"node": "src/main.ts", "dir": "both", "depth": 2}),
        ),
        ("impact", json!({"nodes": ["src/core/model.ts"]})),
        ("find", json!({"query": "service"})),
        ("find", json!({"query": "Service", "kind": "sym"})),
        (
            "path",
            json!({"from": "src/main.ts", "to": "src/core/model.ts"}),
        ),
        ("symbols", json!({"node": "src/core/service.ts"})),
        ("hierarchy", json!({"symbol": "Model"})),
        ("entries", json!({})),
        ("findings", json!({"severity": "info"})),
        ("check", json!({})),
        ("hotspots", json!({})),
        ("hotspots", json!({"within": "src"})),
    ];
    for (tool, args) in calls {
        let r = c.call(tool, args.clone(), T);
        assert_eq!(r["isError"], false, "{tool} {args}: {r}");
        assert!(!text(&r).is_empty(), "{tool}");
        // Budgets hold over the protocol too (1.5k tokens at most, 4 chars each).
        assert!(
            text(&r).len() <= 1600 * 4,
            "{tool}: {} chars",
            text(&r).len()
        );
    }
    // `cartograph query …` hints are rewritten to tool names.
    let r = c.call("overview", json!({}), T);
    assert!(!text(&r).contains("cartograph query"), "{}", text(&r));

    // Actionable errors.
    let r = c.call("explain", json!({"node": "src/core/modle.ts"}), T);
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("did you mean"), "{}", text(&r));
    assert!(text(&r).contains("src/core/model.ts"), "{}", text(&r));
    assert_eq!(r["structuredContent"]["error"]["code"], "unknown_node");
    let r = c.call("deps", json!({"node": "src/main.ts", "dir": "sideways"}), T);
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("out, in or both"), "{}", text(&r));
    let r = c.call("find", json!({"query": "x", "kind": "planet"}), T);
    assert_eq!(r["isError"], true, "{r}");
    let r = c.call("findings", json!({"kind": ["nope"]}), T);
    assert_eq!(r["isError"], true, "{r}");
    // Wrong types and missing fields: still a tool error with the reason, not a crash.
    let r = c.call("explain", json!({"node": 5}), T);
    assert_eq!(r["isError"], true, "{r}");
    let r = c.call("explain", json!({}), T);
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("node"), "{r}");
    // No git repository around the fixture: diff and check(base) say so, as data.
    for (tool, args) in [("diff", json!({})), ("check", json!({"base": "main"}))] {
        let r = c.call(tool, args, T);
        assert_eq!(r["isError"], true, "{tool}: {r}");
        assert!(text(&r).contains("git"), "{tool}: {}", text(&r));
    }
    let r = c.request("tools/call", json!({"name": "nope", "arguments": {}}), T);
    assert!(r["error"].is_object(), "{r}");

    let st = c.shutdown(Duration::from_secs(10));
    assert!(st.success(), "{st:?}");
}

/// Some clients show the model only `structuredContent` when both it and the text are
/// there, so the default is text only.
#[test]
fn json_only_when_asked() {
    let (_tmp, root, data) = fixture("query-app");
    let mut c = spawn(&root, &data, &[]);
    c.handshake();
    let r = call_ready(&mut c, "explain", json!({"node": "src/main.ts"}));
    assert!(r.get("structuredContent").is_none(), "{r}");
    assert!(text(&r).contains("src/main.ts"));
    let r = c.call("explain", json!({"node": "src/nope.ts"}), T);
    assert!(r.get("structuredContent").is_none(), "{r}");
    c.shutdown(Duration::from_secs(10));
}

/// A repo big enough that the first index is still running when the first call arrives.
#[test]
fn starts_without_an_index_and_says_indexing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("big");
    for d in 0..40 {
        let dir = root.join(format!("pkg{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..60 {
            let body = format!(
                "import {{ v{prev} }} from './m{prev}';\nexport function v{f}() {{ return v{prev}() + {f}; }}\n",
                prev = (f as u32).saturating_sub(1)
            );
            std::fs::write(dir.join(format!("m{f}.ts")), body).unwrap();
        }
    }
    let data = tmp.path().join("data");
    let t0 = Instant::now();
    let mut c = spawn(&root, &data, &["--fresh-wait-ms", "0", "--structured"]);
    c.handshake();
    let handshake_ms = t0.elapsed().as_millis();
    assert!(handshake_ms < 10_000, "handshake took {handshake_ms} ms");
    let r = c.call("overview", json!({}), T);
    assert_eq!(r["isError"], false, "{r}");
    let first = text(&r);
    assert!(first.contains("is indexing"), "{first}");
    assert_eq!(r["structuredContent"]["status"], "indexing", "{r}");
    // It answers for real once the index is there.
    let r = call_ready(&mut c, "overview", json!({}));
    assert!(text(&r).contains("pkg"), "{}", text(&r));
    // Closing stdin ends the server promptly.
    let st = c.shutdown(Duration::from_secs(10));
    assert!(st.success(), "{st:?}");
}

#[test]
fn closing_stdin_during_the_first_index_exits_promptly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("big");
    for d in 0..30 {
        let dir = root.join(format!("p{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..60 {
            std::fs::write(
                dir.join(format!("f{f}.py")),
                format!("import os\nX = {f}\n"),
            )
            .unwrap();
        }
    }
    let mut c = spawn(&root, &tmp.path().join("data"), &[]);
    c.handshake();
    let t0 = Instant::now();
    let st = c.shutdown(Duration::from_secs(10));
    assert!(st.success(), "{st:?}");
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
}

/// An edit is visible to the very next call that names the file, with no watcher.
#[test]
fn a_named_file_is_reindexed_before_answering() {
    let (_tmp, root, data) = fixture("query-app");
    let mut c = spawn(&root, &data, &["--no-watch"]);
    c.handshake();
    let before = text(&call_ready(
        &mut c,
        "deps",
        json!({"node": "src/dead.ts", "dir": "out"}),
    ));
    assert!(!before.contains("db.ts"), "{before}");
    let f = root.join("src/dead.ts");
    let mut body = std::fs::read_to_string(&f).unwrap();
    body.insert_str(0, "import { connect } from './infra/db';\n");
    std::fs::write(&f, body).unwrap();
    let after = text(&c.call("deps", json!({"node": "src/dead.ts", "dir": "out"}), T));
    assert!(after.contains("infra/db.ts"), "{after}");
    // A new file is found too.
    std::fs::write(
        root.join("src/fresh.ts"),
        "import { Model } from './core/model';\n",
    )
    .unwrap();
    let r = c.call("explain", json!({"node": "src/fresh.ts"}), T);
    assert_eq!(r["isError"], false, "{r}");
    c.shutdown(Duration::from_secs(10));
}

#[test]
fn notes_persist_and_go_stale_when_the_code_changes() {
    let (_tmp, root, data) = fixture("query-app");
    let mut c = spawn(&root, &data, &[]);
    c.handshake();
    let r = call_ready(
        &mut c,
        "annotate",
        json!({"node": "src/core/service.ts", "text": "Business rules; the only writer of repo."}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let r = c.call(
        "annotate",
        json!({"node": "src/core", "text": "x".repeat(700)}),
        T,
    );
    assert_eq!(r["isError"], true, "too long: {r}");
    let r = c.call("annotate", json!({"node": "npm:nope-lib", "text": "x"}), T);
    assert_eq!(r["isError"], true, "{r}");
    c.shutdown(Duration::from_secs(10));

    // A new server (a new session) shows it.
    let mut c = spawn(&root, &data, &[]);
    c.handshake();
    let t = text(&call_ready(
        &mut c,
        "explain",
        json!({"node": "src/core/service.ts"}),
    ));
    assert!(t.contains("note: Business rules"), "{t}");
    // The CLI shows it too.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(["query", "explain", "src/core/service.ts", "--data-dir"])
        .arg(&data)
        .arg("-C")
        .arg(&root)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("note: Business rules"));
    // Edit the file: the note is marked stale.
    let f = root.join("src/core/service.ts");
    let body = std::fs::read_to_string(&f).unwrap() + "\nexport const added = 1;\n";
    std::fs::write(&f, body).unwrap();
    let t = text(&c.call("explain", json!({"node": "src/core/service.ts"}), T));
    assert!(t.contains("note (stale"), "{t}");
    // Empty text deletes it.
    c.call(
        "annotate",
        json!({"node": "src/core/service.ts", "text": ""}),
        T,
    );
    let t = text(&c.call("explain", json!({"node": "src/core/service.ts"}), T));
    assert!(t.contains("note: none"), "{t}");
    c.shutdown(Duration::from_secs(10));
}

#[test]
fn notes_in_the_repo_file() {
    let (_tmp, root, data) = fixture("query-app");
    let notes = root.join("cartograph-notes.toml");
    // Asked for with a flag: the first note creates the versioned file.
    let mut c = spawn(&root, &data, &["--notes-in-repo"]);
    c.handshake();
    let r = call_ready(
        &mut c,
        "annotate",
        json!({"node": "src/core", "text": "Domain core.\nNo I/O here."}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert!(text(&r).contains("cartograph-notes.toml"), "{r}");
    let body = std::fs::read_to_string(&notes).unwrap();
    assert!(body.contains("[\"dir:src/core\"]"), "{body}");
    assert!(body.contains("No I/O here."), "{body}");
    // Writing the file is no change to the map: the note is fresh.
    let t = text(&c.call("explain", json!({"node": "src/core"}), T));
    assert!(t.contains("note: Domain core."), "{t}");
    assert!(!t.contains("stale"), "{t}");
    c.shutdown(Duration::from_secs(10));

    // Without the flag, the existing file is where notes go; explain reads both places.
    let mut c = spawn(&root, &data, &[]);
    c.handshake();
    let r = call_ready(
        &mut c,
        "annotate",
        json!({"node": "src/core/service.ts", "text": "Business rules."}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let body = std::fs::read_to_string(&notes).unwrap();
    assert!(body.contains("file:src/core/service.ts"), "{body}");
    assert!(!data.join("annotations.json").exists());
    let t = text(&c.call("explain", json!({"node": "src/core/service.ts"}), T));
    assert!(t.contains("note: Business rules."), "{t}");
    // A delete removes it from the file.
    c.call(
        "annotate",
        json!({"node": "src/core/service.ts", "text": ""}),
        T,
    );
    let body = std::fs::read_to_string(&notes).unwrap();
    assert!(!body.contains("service.ts"), "{body}");
    assert!(body.contains("dir:src/core"), "{body}");
    c.shutdown(Duration::from_secs(10));
}
