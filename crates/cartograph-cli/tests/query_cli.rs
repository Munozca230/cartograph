//! `cartograph query`, `check` and `infer` on an index of the `query-app` fixture.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            fs::copy(entry.path(), &dst).unwrap();
        }
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(args)
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn query_app() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/query-app")
}

#[test]
fn query_check_and_infer_read_an_existing_index() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("query-app");
    copy_dir(&query_app(), &root);
    let data = tmp.path().join("db");
    let (r, d) = (root.to_str().unwrap(), data.to_str().unwrap());

    // Without an index every command says how to make one.
    for args in [
        vec!["query", "-C", r, "--data-dir", d, "overview"],
        vec!["infer", r, "--data-dir", d],
    ] {
        let o = run(&args);
        assert!(!o.status.success(), "{args:?}");
        let err = stderr(&o);
        assert!(
            err.contains("no index at") && err.contains("cartograph index"),
            "{err}"
        );
    }
    let o = run(&["check", r, "--data-dir", d]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "check without an index cannot run"
    );

    assert!(run(&["index", r, "--data-dir", d]).status.success());

    // Text by default, JSON with --json; options go before or after the tool.
    let o = run(&["query", "overview", "-C", r, "--data-dir", d]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).starts_with("query-app · "), "{}", stdout(&o));
    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "--json",
        "explain",
        "src/core/model.ts",
    ]);
    assert!(o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["id"], "file:src/core/model.ts");
    assert_eq!(v["layer"], "core");
    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "impact",
        "src/core/model.ts",
        "src/ui",
    ]);
    assert!(stdout(&o).contains("tests to run"), "{}", stdout(&o));
    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "findings",
        "--kind",
        "orphan",
        "--severity",
        "warning",
    ]);
    assert!(stdout(&o).starts_with("2 findings"), "{}", stdout(&o));
    for args in [
        vec!["path", "src/ui", "src/infra"],
        vec!["deps", "src/ui/app.ts", "--dir", "both", "--depth", "2"],
        vec!["find", "serv"],
    ] {
        let mut all = vec!["query", "-C", r, "--data-dir", d];
        all.extend(&args);
        let o = run(&all);
        assert!(o.status.success(), "{args:?}: {}", stderr(&o));
    }

    // An unknown id is an error with suggestions, as text and as JSON.
    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "explain",
        "src/core/modle.ts",
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("did you mean: src/core/model.ts"),
        "{}",
        stderr(&o)
    );
    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "--json",
        "explain",
        "nope",
    ]);
    assert_eq!(o.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["error"]["code"], "unknown_node");

    // The symbol layer: symbols, callers (by bare name, `path#name` or id), hierarchy,
    // entries, symbol search; a misspelled symbol suggests the right one.
    for args in [
        vec!["symbols", "src/core"],
        vec!["callers", "Model"],
        vec!["callers", "src/core/service.ts#run"],
        vec!["callers", "sym:src/core/repo.ts#save"],
        vec!["hierarchy", "Model"],
        vec!["entries"],
        vec!["find", "run", "--kind", "sym"],
    ] {
        let mut full = vec!["query", "-C", r, "--data-dir", d];
        full.extend(args.iter().copied());
        let o = run(&full);
        assert!(o.status.success(), "{args:?}: {}", stderr(&o));
        full.push("--json");
        let o = run(&full);
        assert!(o.status.success(), "{args:?}: {}", stderr(&o));
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap();
    }
    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "callers",
        "src/ui/app.ts#strat",
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("sym:src/ui/app.ts#start"),
        "{}",
        stderr(&o)
    );
    let o = run(&["query", "-C", r, "--data-dir", d, "callers", "Model"]);
    assert!(stdout(&o).contains("callers of sym:src/core/model.ts#Model"));

    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "deps",
        "src/main.ts",
        "--depth",
        "9",
    ]);
    assert_eq!(o.status.code(), Some(1));

    // check: the fixture breaks its rules (exit 1); without rules it passes (exit 0); a bad
    // config cannot be checked (exit 2, with line and key).
    let o = run(&["check", r, "--data-dir", d]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stdout(&o).contains("FAIL: 5 rule violations"),
        "{}",
        stdout(&o)
    );
    let o = run(&["check", r, "--data-dir", d, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["ok"], false);
    let empty = tmp.path().join("empty.toml");
    fs::write(&empty, "").unwrap();
    let o = run(&[
        "check",
        r,
        "--data-dir",
        d,
        "--config",
        empty.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    let bad = tmp.path().join("bad.toml");
    fs::write(&bad, "[rules]\nmax_fan_out = \"many\"\n").unwrap();
    let o = run(&[
        "check",
        r,
        "--data-dir",
        d,
        "--config",
        bad.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("bad.toml:2"), "{}", stderr(&o));
    // query still answers with the defaults, and says why.
    let o = run(&[
        "query",
        "-C",
        r,
        "--data-dir",
        d,
        "--config",
        bad.to_str().unwrap(),
        "find",
        "model",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("warning: invalid config"),
        "{}",
        stderr(&o)
    );
    assert!(stdout(&o).contains("src/core/model.ts"));

    // infer prints a proposal and writes nothing without --write.
    fs::remove_file(root.join("cartograph.toml")).unwrap();
    let o = run(&["infer", r, "--data-dir", d]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("[[layers]]"));
    assert!(!root.join("cartograph.toml").exists());
    let o = run(&["infer", r, "--data-dir", d, "--write"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(root.join("cartograph.toml").is_file());
    let o = run(&["infer", r, "--data-dir", d, "--write"]);
    assert!(!o.status.success(), "refuses to overwrite without --force");
    // The written proposal passes check.
    let o = run(&["check", r, "--data-dir", d]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
}

#[test]
fn infer_write_refuses_a_config_it_cannot_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("query-app");
    copy_dir(&query_app(), &root);
    let data = tmp.path().join("db");
    let (r, d) = (root.to_str().unwrap(), data.to_str().unwrap());
    assert!(run(&["index", r, "--data-dir", d]).status.success());
    let file = root.join("cartograph.toml");
    // Not TOML (an unclosed table header further down), with an `ignore` that would be lost.
    let broken = "ignore = [\"src/ui/**\"]\n\n[layers\nname = \"x\"\n";
    fs::write(&file, broken).unwrap();
    let o = run(&["infer", r, "--data-dir", d, "--write", "--force"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("not overwriting"), "{}", stderr(&o));
    assert_eq!(fs::read_to_string(&file).unwrap(), broken);
    // Without --write it only proposes: a warning, and the proposal.
    let o = run(&["infer", r, "--data-dir", d]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("warning"), "{}", stderr(&o));
    // An `ignore` that is not a list of patterns would be lost too.
    let bad_ignore = "ignore = 3\n";
    fs::write(&file, bad_ignore).unwrap();
    let o = run(&["infer", r, "--data-dir", d, "--write", "--force"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(fs::read_to_string(&file).unwrap(), bad_ignore);
    // Not readable as text.
    let bytes = b"ignore = [\"a\"]\n\xff\n";
    fs::write(&file, bytes).unwrap();
    let o = run(&["infer", r, "--data-dir", d, "--write", "--force"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(fs::read(&file).unwrap(), bytes);
}

#[test]
fn infer_keeps_the_ignore_of_a_broken_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("query-app");
    copy_dir(&query_app(), &root);
    let data = tmp.path().join("db");
    let (r, d) = (root.to_str().unwrap(), data.to_str().unwrap());
    assert!(run(&["index", r, "--data-dir", d]).status.success());
    // A config being replaced: its layer is broken, its ignore still counts.
    fs::write(
        root.join("cartograph.toml"),
        "ignore = [\"src/ui/**\"]\n\n[[layers]]\nname = \"x\"\n",
    )
    .unwrap();
    let o = run(&["infer", r, "--data-dir", d]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("ignore = [\"src/ui/**\"]"), "{out}");
    // 12 code files without the ignore; src/ui holds two of them.
    assert!(out.contains("# 10 files"), "{out}");
    let o = run(&["infer", r, "--data-dir", d, "--write", "--force"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = run(&["check", r, "--data-dir", d]);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
}
