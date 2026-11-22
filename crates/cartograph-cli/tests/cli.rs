//! CLI smoke test: index a temp copy of the polyglot fixture, then report and doctor.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/polyglot")
}

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

fn run(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(args)
        .arg(path)
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn index_then_report() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("polyglot");
    copy_dir(&fixture(), &root);

    // Report before indexing fails with a clear message.
    let o = run(&["report"], &root);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("cartograph index"));

    // -v turns on info logs; they must go to stderr, never stdout.
    let o = run(&["-v", "index"], &root);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(root.join(".cartograph/graph.db").is_file());
    let out = stdout(&o);
    assert_eq!(
        out.lines().count(),
        2,
        "stdout must hold only the summary and the git line: {out}"
    );
    assert!(out.starts_with("indexed 31 files"), "{out}");
    // A folder outside any repository has no history, and that is no error.
    assert_eq!(
        out.lines().nth(1),
        Some("git: not a git repository (no history)"),
        "{out}"
    );
    // TS/JS and Python: 3; Rust `mod lib;`, C `"util.h"`, C++ `"engine.hpp"`: 3;
    // Go `net/http` and `fmt`, C# `using System;`: 3; Godot: 6 (scene script and theme,
    // main scene, autoload, two built-in base classes).
    assert!(out.contains("15 imports (0 unresolved)"), "{out}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("indexed"));
    let o = run(&["index", "--no-git"], &root);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(stdout(&o).lines().count(), 1, "{}", stdout(&o));

    let o = run(&["report"], &root);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let md = stdout(&o);
    for needle in [
        "# cartograph report: polyglot",
        "## Languages",
        "| python | 4 |",
        "| gdscript | 2 |",
        "## Manifests and packages",
        "| game/project.godot | godot | Polyglot Game | autoloads: GameState; main scene: res://main.tscn |",
        "| package.json | npm | polyglot-web | workspaces: web |",
        "| Cargo.toml | cargo | polyglot-rs |",
        "## Top directories by LOC",
        "| game | 5 |",
        "| web | 7 |",
        // The data dir exists during the walk (it is created first) and is excluded.
        "## Skipped (2)",
        "| .cartograph | excluded_dir |",
        "| assets/logo.png | binary |",
        "## Index timing",
        "| total |",
    ] {
        assert!(md.contains(needle), "missing {needle:?} in report:\n{md}");
    }
}

/// `stats --days` with a huge number means "everything", not an overflow panic.
#[test]
fn stats_with_huge_days_does_not_overflow() {
    let tmp = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(["stats", "--days", &u64::MAX.to_string()])
        .arg(tmp.path())
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn doctor_passes_in_a_writable_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let o = run(&["doctor"], tmp.path());
    let out = stdout(&o);
    assert!(o.status.success(), "{out}");
    for check in [
        "binary",
        "data dir writable",
        "sqlite wal",
        "grammars",
        "existing index",
    ] {
        assert!(
            out.lines()
                .any(|l| l.starts_with("PASS") && l.contains(check)),
            "{check}: {out}"
        );
    }
    assert!(out.contains("11 grammars load"));
    // The probe database is cleaned up.
    let leftovers: Vec<_> = fs::read_dir(tmp.path().join(".cartograph"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn doctor_fails_on_a_missing_path_without_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let o = run(&["doctor"], &missing);
    let out = stdout(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("FAIL") && l.contains("repo path")),
        "{out}"
    );
    assert!(!missing.exists(), "doctor created {}", missing.display());
}

#[test]
fn doctor_fails_when_the_data_dir_cannot_be_created() {
    let tmp = tempfile::tempdir().unwrap();
    // A regular file where the repo directory should be: nothing can be created under it.
    let not_a_dir = tmp.path().join("file");
    fs::write(&not_a_dir, b"x").unwrap();
    let o = run(&["doctor"], &not_a_dir);
    let out = stdout(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("FAIL") && l.contains("data dir writable")),
        "{out}"
    );
}

/// Every path under `root`; files with their size and modification time. Directory
/// timestamps are left out: NTFS may update them lazily after the copy that built the
/// tree, and any file created in a directory shows up as a new path anyway.
fn snapshot(root: &Path) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
                out.push((entry.path(), 0, None));
            } else {
                out.push((entry.path(), meta.len(), Some(meta.modified().unwrap())));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn data_dir_keeps_the_analyzed_repo_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("polyglot");
    copy_dir(&fixture(), &root);
    let data = tmp.path().join("elsewhere").join("db");
    let data_arg = data.to_str().unwrap();
    let before = snapshot(&root);

    let o = run(&["index", "--data-dir", data_arg], &root);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(data.join("graph.db").is_file());
    let o = run(&["report", "--data-dir", data_arg], &root);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(stdout(&o).contains("## Imports"));
    let o = run(&["doctor", "--data-dir", data_arg], &root);
    assert!(o.status.success(), "{}", stdout(&o));
    let schema = format!("schema v{}", cartograph_core::SCHEMA_VERSION);
    assert!(stdout(&o).contains(&schema), "{}", stdout(&o));

    assert!(!root.join(".cartograph").exists());
    assert_eq!(snapshot(&root), before, "the analyzed repo was modified");
}

#[test]
fn report_hint_mentions_the_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let o = run(&["report", "--data-dir", "somewhere"], tmp.path());
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--data-dir somewhere"));
}

#[test]
fn serve_on_a_taken_explicit_port_exits_with_an_error() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let tmp = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_cartograph"))
        .args(["serve", "--no-watch", "--port", &port, "--data-dir"])
        .arg(tmp.path().join("db"))
        .arg(fixture())
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains(&format!("port {port}")), "{err}");
    assert!(o.stdout.is_empty(), "nothing on stdout");
}
