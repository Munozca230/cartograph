//! `cartograph diff` and `check --base` on a synthetic git repository: a base commit
//! with existing violations, a branch that introduces a cycle and a layer violation, and
//! uncommitted work that adds one more import across an already violated layer pair and an
//! unresolved import. Commits have fixed dates and people, so ids (and goldens) are stable.
//!
//! Regenerate the goldens with `UPDATE_GOLDEN=1 cargo test -p cartograph-cli --test diff_base`.

#[path = "../../cartograph-index/tests/support/git_fixture.rs"]
mod git_fixture;
mod mcp_client;

use git_fixture::{ADA, GitRepo};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOML: &str = r#"[[layers]]
name = "ui"
paths = ["src/ui/**"]
forbid = ["db"]

[[layers]]
name = "core"
paths = ["src/core/**"]
forbid = ["ui"]

[[layers]]
name = "db"
paths = ["src/db/**"]

[rules]
no_cycles = true
"#;

/// The scenario, on branch `feature` with uncommitted work on top.
fn scenario() -> GitRepo {
    let r = GitRepo::new();
    r.write("cartograph.toml", TOML);
    r.write("src/db/store.ts", "export const store = 1;\n");
    r.write(
        "src/core/model.ts",
        "import { store } from \"../db/store\";\nexport const model = store;\n",
    );
    r.write(
        "src/ui/view.ts",
        "import { model } from \"../core/model\";\nimport { store } from \"../db/store\";\nexport const view = [model, store];\n",
    );
    r.write("src/ui/widget.ts", "export const widget = 2;\n");
    // An old cycle: a violation already in the base.
    r.write(
        "src/legacy/x.ts",
        "import { y } from \"./y\";\nexport const x = () => y;\n",
    );
    r.write(
        "src/legacy/y.ts",
        "import { x } from \"./x\";\nexport const y = () => x;\n",
    );
    r.commit(ADA, 0, "base");

    // The branch: a new cycle (model ↔ service) and a new forbidden pair (core → ui).
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write(
        "src/core/service.ts",
        "import { model } from \"./model\";\nimport { widget } from \"../ui/widget\";\nexport const service = [model, widget];\n",
    );
    r.write(
        "src/core/model.ts",
        "import { store } from \"../db/store\";\nimport { service } from \"./service\";\nexport const model = [store, service];\n",
    );
    r.commit(ADA, 1, "feature");

    // Uncommitted: one more ui → db import (the pair is violated in the base already), an
    // unresolved import, and a file removed.
    r.write(
        "src/ui/panel.ts",
        "import { store } from \"../db/store\";\nexport const panel = store;\n",
    );
    r.write(
        "src/ui/view.ts",
        "import { model } from \"../core/model\";\nimport { store } from \"../db/store\";\nimport { gone } from \"./missing\";\nexport const view = [model, store, gone];\n",
    );
    std::fs::remove_file(r.root.join("src/ui/widget.ts")).ok();
    r.write("src/ui/widget.ts", "export const widget = 3;\n");
    r
}

fn run(root: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(args)
        .arg("--data-dir")
        .arg(data)
        .current_dir(root)
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn golden(rel: &str, actual: &str) {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden")
        .join(rel);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "missing golden {}; run with UPDATE_GOLDEN=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(actual, expected, "golden {rel} differs");
}

/// Refs, HEAD and the status of the repo: none of it may change.
fn repo_state(r: &GitRepo) -> String {
    format!(
        "{}\n{}\n{}",
        r.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
        r.git(&["rev-parse", "HEAD"]),
        r.git(&["status", "--porcelain", "--untracked-files=all"])
    )
}

#[test]
fn check_base_fails_only_on_what_the_change_introduces() {
    let r = scenario();
    let data = tempfile::tempdir().unwrap();
    let before = repo_state(&r);

    // Without a base every violation fails, the old cycle included.
    let o = run(&r.root, data.path(), &["index", "."]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = run(&r.root, data.path(), &["check", "."]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(stdout(&o).contains("src/legacy/"), "{}", stdout(&o));

    // Against main: the new cycle (and the folder cycle src/core ↔ src/ui it makes), the
    // new core → ui pair, and the one new ui → db import (uncommitted) fail; the old cycle
    // is existing.
    let o = run(&r.root, data.path(), &["check", ".", "--base", "main"]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}\n{}", stderr(&o));
    golden("diff/check-base-main.txt", &out);
    assert!(out.contains("FAIL: 4 new rule violations"), "{out}");
    assert!(out.contains("layer_violation:core->ui"), "{out}");
    assert!(out.contains("cycle:file:src/core/model.ts"), "{out}");
    assert!(out.contains("folder_cycle:dir:src/core"), "{out}");
    assert!(
        out.contains("src/core/service.ts:2 → src/ui/widget.ts"),
        "{out}"
    );
    assert!(out.contains("src/ui/panel.ts:1 → src/db/store.ts"), "{out}");
    // Only the new evidence of the worse pair: the import the base has is not listed.
    assert!(!out.contains("src/ui/view.ts:2 → src/db/store.ts"), "{out}");
    assert!(
        out.contains("existing (in the base too, not failing): 1"),
        "{out}"
    );
    assert!(out.contains("src/legacy/"), "{out}");

    let o = run(
        &r.root,
        data.path(),
        &["check", ".", "--base", "main", "--json"],
    );
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["violations"].as_array().unwrap().len(), 4, "{v}");
    assert_eq!(v["baseline_violations"], 1, "{v}");

    // Against the branch's own commit only the uncommitted import is new.
    let o = run(&r.root, data.path(), &["check", ".", "--base", "HEAD"]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("FAIL: 1 new rule violations"), "{out}");
    assert!(out.contains("src/ui/panel.ts:1 → src/db/store.ts"), "{out}");

    // Without the uncommitted import nothing is new: every violation is existing, exit 0
    // (no `index` in between: check --base re-indexes the working tree itself).
    std::fs::remove_file(r.root.join("src/ui/panel.ts")).unwrap();
    let o = run(&r.root, data.path(), &["check", ".", "--base", "HEAD"]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("PASS: no new rule violations"), "{out}");
    assert!(
        out.contains("existing (in the base too, not failing): 5"),
        "{out}"
    );
    r.write(
        "src/ui/panel.ts",
        "import { store } from \"../db/store\";\nexport const panel = store;\n",
    );

    // An unknown base cannot be checked: exit 2, not 1.
    let o = run(&r.root, data.path(), &["check", ".", "--base", "nope"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("nope"), "{}", stderr(&o));
    // Nor an empty one (it is not the merge-base).
    for args in [
        &["check", ".", "--base", ""][..],
        &["check", ".", "--base", " "],
        &["diff", ""],
    ] {
        let o = run(&r.root, data.path(), args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stdout(&o));
        assert!(stderr(&o).contains("empty"), "{args:?}: {}", stderr(&o));
    }

    // Nothing in the repository changed: no checkout, no worktree, no ref, no file.
    assert_eq!(repo_state(&r), before);
}

#[test]
fn diff_golden_and_cache() {
    let r = scenario();
    let data = tempfile::tempdir().unwrap();
    let before = repo_state(&r);

    let o = run(&r.root, data.path(), &["diff", "main"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    golden("diff/diff-main.txt", &text);
    assert!(text.contains("rule violations introduced: 4"), "{text}");
    assert!(text.contains("files: +2 −0"), "{text}");
    assert!(text.contains("\"./missing\""), "{text}");

    // The base is cached by commit: the second run builds nothing.
    let snaps: Vec<_> = std::fs::read_dir(data.path().join(".cartograph-base"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(snaps.len(), 1, "{snaps:?}");
    assert!(snaps[0].ends_with(".json"), "{snaps:?}");
    let o = run(&r.root, data.path(), &["-v", "diff", "main", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("cached=true"), "{}", stderr(&o));
    let json = stdout(&o);
    let v: Value = serde_json::from_str(&json).unwrap();
    golden(
        "diff/diff-main.json",
        &(serde_json::to_string_pretty(&v).unwrap() + "\n"),
    );
    assert_eq!(v["files_added"], 2);
    assert_eq!(v["unresolved_new"][0]["specifier"], "./missing");

    // No base given: the merge-base with main.
    let o = run(&r.root, data.path(), &["diff"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).starts_with("diff merge-base with main ("),
        "{}",
        stdout(&o)
    );

    // The diff of the base with itself is empty.
    r.git(&["stash", "push", "-q", "-u", "-m", "t10-fixture"]);
    r.git(&["checkout", "-q", "main"]);
    let o = run(&r.root, data.path(), &["diff", "HEAD"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("no architectural change"),
        "{}",
        stdout(&o)
    );
    r.git(&["checkout", "-q", "feature"]);
    r.git(&["stash", "pop", "-q"]);

    assert_eq!(repo_state(&r), before);
}

/// A dependency the rules do not count (on a test file, here) does not make an existing
/// `max_fan_out` violation worse; one more dependency on code does.
#[test]
fn check_base_ignores_dependencies_the_rules_do_not_count() {
    let r = GitRepo::new();
    r.write("cartograph.toml", "[rules]\nmax_fan_out = 1\n");
    r.write("src/a.ts", "export const a = 1;\n");
    r.write("src/b.ts", "export const b = 2;\n");
    r.write("src/c.ts", "export const c = 3;\n");
    r.write("src/fixtures.test.ts", "export const sample = 4;\n");
    r.write(
        "src/app.ts",
        "import { a } from \"./a\";\nimport { b } from \"./b\";\nexport const app = [a, b];\n",
    );
    r.commit(ADA, 0, "base");
    let data = tempfile::tempdir().unwrap();

    r.write(
        "src/app.ts",
        "import { a } from \"./a\";\nimport { b } from \"./b\";\nimport { sample } from \"./fixtures.test\";\nexport const app = [a, b, sample];\n",
    );
    let o = run(&r.root, data.path(), &["check", ".", "--base", "HEAD"]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(0), "{out}\n{}", stderr(&o));
    assert!(out.contains("PASS: no new rule violations"), "{out}");
    assert!(
        out.contains("existing (in the base too, not failing): 1"),
        "{out}"
    );

    r.write(
        "src/app.ts",
        "import { a } from \"./a\";\nimport { b } from \"./b\";\nimport { c } from \"./c\";\nexport const app = [a, b, c];\n",
    );
    let o = run(&r.root, data.path(), &["check", ".", "--base", "HEAD"]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}\n{}", stderr(&o));
    assert!(out.contains("src/app.ts:3 → src/c.ts"), "{out}");
}

/// Paths of the working-tree index skipped as too large.
fn too_large(data: &Path) -> Vec<String> {
    let store = cartograph_core::Store::open(data.join("graph.db")).unwrap();
    store
        .load_graph()
        .unwrap()
        .skipped
        .into_iter()
        .filter(|s| s.reason == cartograph_core::SkipReason::TooLarge)
        .map(|s| s.path)
        .collect()
}

fn indexed(data: &Path, path: &str) -> bool {
    let store = cartograph_core::Store::open(data.join("graph.db")).unwrap();
    store
        .load_graph()
        .unwrap()
        .nodes_of(cartograph_core::NodeKind::File)
        .any(|n| n.path == path)
}

/// The size limit the stored index records.
fn stored_limit(data: &Path) -> Option<String> {
    let store = cartograph_core::Store::open(data.join("graph.db")).unwrap();
    store.get_meta("max_file_bytes").unwrap()
}

/// `diff` and `check --base` build the working tree in memory and never write the index:
/// new files show in the diff but not in the index, a given `--max-file-bytes` applies to
/// the run only (the stored limit applies otherwise), and without an index nothing is
/// created.
#[test]
fn diff_and_check_base_never_write_the_index() {
    let r = GitRepo::new();
    r.write(
        "src/a.ts",
        "export const a = 1;
",
    );
    r.commit(ADA, 0, "base");
    let data = tempfile::tempdir().unwrap();
    let o = run(
        &r.root,
        data.path(),
        &["index", ".", "--max-file-bytes", "100"],
    );
    assert!(o.status.success(), "{}", stderr(&o));

    // Uncommitted: a file over the stored limit and a small one.
    r.write(
        "src/big.ts",
        &format!(
            "export const big = \"{}\";
",
            "x".repeat(200)
        ),
    );
    r.write(
        "src/c.ts",
        "export const c = 3;
",
    );
    let o = run(&r.root, data.path(), &["diff", "HEAD"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("files: +1 −0"), "{}", stdout(&o));
    let o = run(
        &r.root,
        data.path(),
        &["diff", "HEAD", "--max-file-bytes", "1000"],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("files: +2 −0"), "{}", stdout(&o));
    let o = run(&r.root, data.path(), &["check", ".", "--base", "HEAD"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));

    assert!(indexed(data.path(), "src/a.ts"));
    assert!(!indexed(data.path(), "src/c.ts"));
    assert!(too_large(data.path()).is_empty());
    assert_eq!(stored_limit(data.path()).as_deref(), Some("100"));

    // No index: the diff still runs, with the default limit, and creates none.
    let empty = tempfile::tempdir().unwrap();
    let o = run(&r.root, empty.path(), &["diff", "HEAD"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("files: +2 −0"), "{}", stdout(&o));
    assert!(!empty.path().join("graph.db").exists());
}

/// With the data dir as the repo root, a source folder named `base` (the snapshot folder's
/// former name) is indexed and compared like any other, and a diff deletes nothing in it,
/// not even a file named like a snapshot of an older version. A `base` holding only what
/// an older version left there is left out, and kept too.
#[test]
fn a_base_source_folder_next_to_the_data_files_is_code() {
    let r = GitRepo::new();
    r.write("base/x.ts", "export const x = 1;\n");
    r.commit(ADA, 0, "base");
    r.write(
        "base/y.ts",
        "import { x } from \"./x\";\nexport const y = x;\n",
    );
    let user = "0123456789abcdef0123456789abcdef01234567.json";
    r.write(&format!("base/{user}"), "{}");
    let root = r.root.clone();
    let o = run(&root, &root, &["index", "."]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(indexed(&root, "base/x.ts"));
    let o = run(&root, &root, &["diff", "HEAD"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("base/y.ts"), "{}", stdout(&o));
    assert!(root.join("base/x.ts").is_file() && root.join("base/y.ts").is_file());
    assert!(root.join("base").join(user).is_file());
    assert!(root.join(".cartograph-base").is_dir());

    // Only snapshot artifacts in `base`: neither indexed nor compared, nor deleted.
    let r = GitRepo::new();
    r.write("src/a.ts", "export const a = 1;\n");
    r.commit(ADA, 0, "base");
    let old = "89abcdef0123456789abcdef0123456789abcdef.json";
    r.write(
        &format!("base/{old}"),
        r#"{"key":"k","revision_subject":"s","graph":{}}"#,
    );
    r.write("base/t12-3/src/a.ts", "export const a = 1;\n");
    let root = r.root.clone();
    let o = run(&root, &root, &["index", "."]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(indexed(&root, "src/a.ts"));
    assert!(!indexed(&root, &format!("base/{old}")));
    assert!(!indexed(&root, "base/t12-3/src/a.ts"));
    let o = run(&root, &root, &["diff", "HEAD"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("no architectural change"),
        "{}",
        stdout(&o)
    );
    assert!(root.join("base").join(old).is_file());
    assert!(root.join("base/t12-3/src/a.ts").is_file());
}

/// An unreadable `graph.db` only costs the git history (and the extraction cache): `diff`
/// and `check --base` warn and run without it.
#[test]
fn an_unreadable_index_is_a_warning_for_diff_and_check_base() {
    let r = GitRepo::new();
    r.write("src/a.ts", "export const a = 1;\n");
    r.commit(ADA, 0, "base");
    r.write("src/b.ts", "export const b = 2;\n");
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("graph.db"), "not a database, not at all").unwrap();
    let o = run(&r.root, data.path(), &["diff", "HEAD"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("files: +1 −0"), "{}", stdout(&o));
    assert!(
        stderr(&o).contains("warning: git history not read"),
        "{}",
        stderr(&o)
    );
    let o = run(&r.root, data.path(), &["check", ".", "--base", "HEAD"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
}

/// `index` and `mcp` without `--max-file-bytes` re-index with the limit the stored index
/// was built with, not the 1 MiB default.
#[test]
fn index_and_mcp_keep_the_stored_size_limit() {
    use std::time::{Duration, Instant};
    let r = GitRepo::new();
    r.write(
        "src/big.ts",
        &format!("export const big = \"{}\";\n", "x".repeat(200)),
    );
    r.commit(ADA, 0, "base");
    let data = tempfile::tempdir().unwrap();
    let o = run(
        &r.root,
        data.path(),
        &["index", ".", "--max-file-bytes", "100"],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let o = run(&r.root, data.path(), &["index", "."]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(too_large(data.path()), vec!["src/big.ts".to_string()]);

    // The server's startup index (it sees the new file) keeps the limit too.
    r.write("src/c.ts", "export const c = 3;\n");
    let d = data.path().display().to_string();
    let mut c = mcp_client::Client::spawn(&r.root, &["--no-watch", "--data-dir", &d]);
    c.handshake();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !indexed(data.path(), "src/c.ts") {
        assert!(Instant::now() < deadline, "the server did not re-index");
        std::thread::sleep(Duration::from_millis(50));
    }
    c.shutdown(Duration::from_secs(20));
    assert_eq!(too_large(data.path()), vec!["src/big.ts".to_string()]);
}

/// A cached base snapshot is rebuilt when the global attributes file (it changes how text
/// is checked out) appears, or when `core.attributesFile` names another one. The global
/// config and home of the child are temp dirs.
#[test]
fn the_base_cache_follows_the_global_attributes_file() {
    let r = GitRepo::new();
    r.write("src/a.ts", "export const a = 1;\n");
    r.commit(ADA, 0, "base");
    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cached = || {
        let o = Command::new(env!("CARGO_BIN_EXE_cartograph"))
            .args(["-v", "diff", "HEAD", "--data-dir"])
            .arg(data.path())
            .current_dir(&r.root)
            .env_remove("RUST_LOG")
            .env("XDG_CONFIG_HOME", home.path())
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", stderr(&o));
        stderr(&o).contains("base snapshot from cache")
    };
    assert!(!cached());
    assert!(cached());
    std::fs::create_dir_all(home.path().join("git")).unwrap();
    std::fs::write(home.path().join("git/attributes"), "*.ts text eol=crlf\n").unwrap();
    assert!(!cached(), "a new global attributes file");
    assert!(cached());
    let other = home.path().join("other-attributes");
    std::fs::write(&other, "*.ts -text\n").unwrap();
    r.git(&[
        "config",
        "core.attributesFile",
        &other.display().to_string(),
    ]);
    assert!(!cached(), "another core.attributesFile");
    assert!(cached());
}

/// The MCP tools on the same scenario: `check(base)` and `diff` (default base).
#[test]
fn mcp_diff_and_check_base() {
    use serde_json::json;
    use std::time::{Duration, Instant};
    const T: Duration = Duration::from_secs(60);
    let r = scenario();
    let data = tempfile::tempdir().unwrap();
    let d = data.path().display().to_string();
    let mut c = mcp_client::Client::spawn(&r.root, &["--data-dir", &d]);
    c.handshake();
    // The first index may still run.
    let deadline = Instant::now() + T;
    let check = loop {
        let res = c.call("check", json!({"base": "main"}), T);
        if !mcp_client::text(&res).contains("is indexing") || Instant::now() > deadline {
            break res;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let text = mcp_client::text(&check);
    assert_eq!(check["isError"], false, "{text}");
    assert!(text.contains("FAIL: 4 new rule violations"), "{text}");
    assert!(
        text.contains("src/ui/panel.ts:1 → src/db/store.ts"),
        "{text}"
    );
    assert!(text.contains("cycle:file:src/legacy/x.ts"), "{text}");

    let res = c.call("diff", json!({}), T);
    let text = mcp_client::text(&res);
    assert_eq!(res["isError"], false, "{text}");
    assert!(text.starts_with("diff merge-base with main ("), "{text}");
    assert!(text.contains("rule violations introduced: 4"), "{text}");

    let res = c.call("diff", json!({"base": "no-such-branch"}), T);
    let text = mcp_client::text(&res);
    assert_eq!(res["isError"], true, "{text}");
    assert!(text.contains("no-such-branch"), "{text}");

    // A blank base is a missing one (agents send "" for an optional string): the default
    // base for `diff`, a plain check for `check`.
    for base in ["", "  "] {
        let res = c.call("diff", json!({ "base": base }), T);
        let text = mcp_client::text(&res);
        assert_eq!(res["isError"], false, "{base:?}: {text}");
        assert!(
            text.starts_with("diff merge-base with main ("),
            "{base:?}: {text}"
        );
        let res = c.call("check", json!({ "base": base }), T);
        let text = mcp_client::text(&res);
        assert!(!text.contains("new rule violations"), "{base:?}: {text}");
        assert!(!text.contains("empty"), "{base:?}: {text}");
    }
    assert!(c.shutdown(Duration::from_secs(10)).success());
}

/// An Electron app whose working tree adds an IPC channel (preload and handler) and drops
/// another: `diff` lists the bridges both ways, and an IPC finding that was already in the
/// base is not new (both sides are indexed with their bridges).
#[test]
fn diff_lists_the_bridges_a_change_adds_and_removes() {
    let r = GitRepo::new();
    r.write(
        "package.json",
        "{\"name\":\"app\",\"main\":\"main.js\",\"dependencies\":{\"electron\":\"30.0.0\"}}\n",
    );
    r.write(
        "src/main.ts",
        "import { ipcMain } from 'electron';\nipcMain.handle('a:get', () => 1);\nipcMain.handle('b:get', () => 2);\n",
    );
    r.write(
        "src/preload.ts",
        "import { contextBridge, ipcRenderer } from 'electron';\ncontextBridge.exposeInMainWorld('api', {\n  a: () => ipcRenderer.invoke('a:get'),\n  b: () => ipcRenderer.invoke('b:get'),\n  gone: () => ipcRenderer.invoke('nobody'),\n});\n",
    );
    r.commit(ADA, 0, "base");
    r.write(
        "src/main.ts",
        "import { ipcMain } from 'electron';\nipcMain.handle('a:get', () => 1);\nipcMain.handle('c:get', () => 3);\n",
    );
    r.write(
        "src/preload.ts",
        "import { contextBridge, ipcRenderer } from 'electron';\ncontextBridge.exposeInMainWorld('api', {\n  a: () => ipcRenderer.invoke('a:get'),\n  c: () => ipcRenderer.invoke('c:get'),\n  gone: () => ipcRenderer.invoke('nobody'),\n});\n",
    );
    let data = tempfile::tempdir().unwrap();
    let o = run(&r.root, data.path(), &["diff", "HEAD"]);
    let out = stdout(&o);
    assert!(o.status.success(), "{out}\n{}", stderr(&o));
    assert!(out.contains("bridges: 1 new, 1 removed"), "{out}");
    assert!(
        out.contains("+ ipc invoke \"c:get\" src/preload.ts → src/main.ts"),
        "{out}"
    );
    assert!(
        out.contains("− ipc invoke \"b:get\" src/preload.ts → src/main.ts"),
        "{out}"
    );
    assert!(
        !out.contains("ipc_no_handler"),
        "already in the base: {out}"
    );
}
