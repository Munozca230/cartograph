//! The watcher's event filter agrees with the walker: a change matters exactly when the
//! walker would index that path. Git's own files are never indexed, but the few that mean
//! new history are seen ([`GitWatch`]).

use cartograph_index::walk::{WalkHooks, walk};
use cartograph_server::watch::{GitWatch, WatchFilter};
use std::collections::BTreeSet;

/// Git files that mean new history are seen, in the repo's `.git` and in a linked
/// worktree's git dir and common dir; staging, objects and lock files are not.
#[test]
fn git_watch_sees_history_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let main = base.join("main");
    let git = main.join(".git");
    for d in [
        "refs/heads/feat",
        "refs/tags",
        "logs",
        "objects/ab",
        "worktrees/wt/logs",
    ] {
        std::fs::create_dir_all(git.join(d)).unwrap();
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(git.join("worktrees/wt/HEAD"), "ref: refs/heads/side\n").unwrap();
    std::fs::write(git.join("worktrees/wt/commondir"), "../..\n").unwrap();
    let wt = base.join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", git.join("worktrees/wt").display()),
    )
    .unwrap();

    let w = GitWatch::find(&main).unwrap();
    for (p, yes) in [
        ("HEAD", true),
        ("logs/HEAD", true),
        ("refs/heads/main", true),
        ("refs/heads/feat/x", true),
        ("packed-refs", true),
        ("shallow", true),
        ("refs/heads/main.lock", false),
        ("index", false),
        ("index.lock", false),
        ("objects/ab/cdef", false),
        ("refs/tags/v1", false),
        ("ORIG_HEAD", false),
    ] {
        assert_eq!(w.matches(&git.join(p)), yes, "{p}");
    }
    assert!(w.extra_watches(&main).is_empty(), "inside the root");
    // A root below the work tree's top: its `.git` is outside.
    std::fs::create_dir_all(main.join("sub")).unwrap();
    assert!(!w.extra_watches(&main.join("sub")).is_empty());

    let w = GitWatch::find(&wt).unwrap();
    assert!(w.matches(&git.join("worktrees/wt/HEAD")));
    assert!(w.matches(&git.join("worktrees/wt/logs/HEAD")));
    assert!(w.matches(&git.join("refs/heads/side")));
    assert!(!w.matches(&git.join("HEAD")), "the main worktree's HEAD");
    let extra: Vec<_> = w.extra_watches(&wt).into_iter().map(|(p, _)| p).collect();
    assert!(extra.contains(&git.join("worktrees/wt")), "{extra:?}");
    assert!(extra.contains(&git.join("refs/heads")), "{extra:?}");
}

#[test]
fn the_filter_accepts_exactly_what_the_walker_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let files = [
        ".gitignore",
        ".ignore",
        "a.ts",
        "ignored.ts",
        "build/out.js",
        "x.log",
        "keep.log",
        "secret.ts",
        "excluded.ts",
        "sub/.gitignore",
        "sub/b.ts",
        "sub/local.ts",
        "sub/deeper/c.ts",
        "sub/deeper/local.ts",
        "node_modules/m/index.js",
        "deep/node_modules/m/index.js",
        "target/t.rs",
        "deep/target/x.rs",
        ".cartograph/graph.db-wal",
        "mydata/graph.db",
        "mydata/graph.db-wal",
        "docs/target.md",
    ];
    for f in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x\n").unwrap();
    }
    std::fs::write(
        root.join(".gitignore"),
        "ignored.ts\nbuild/\n*.log\n!keep.log\n",
    )
    .unwrap();
    std::fs::write(root.join(".ignore"), "secret.ts\n").unwrap();
    std::fs::write(root.join("sub/.gitignore"), "local.ts\n").unwrap();
    std::fs::create_dir_all(root.join(".git/info")).unwrap();
    std::fs::write(root.join(".git/info/exclude"), "excluded.ts\n").unwrap();

    let walked = walk(
        &root,
        1 << 20,
        &["mydata".to_string()],
        WalkHooks::default(),
    );
    let indexed: BTreeSet<String> = walked.files.iter().map(|f| f.path.clone()).collect();
    assert!(indexed.contains("a.ts") && indexed.contains("keep.log"));
    assert!(!indexed.contains("ignored.ts") && !indexed.contains("sub/deeper/local.ts"));

    let filter = WatchFilter::new(&root, &root.join("mydata"));
    for f in files {
        let relevant = filter.relevant(&root.join(f)).is_some();
        assert_eq!(relevant, indexed.contains(f), "{f}");
    }
    // The SCIP index at the root matters even when ignored (as it usually is).
    std::fs::write(root.join(".gitignore"), "index.scip\nsub/index.scip\n").unwrap();
    std::fs::write(root.join("index.scip"), "x").unwrap();
    std::fs::write(root.join("sub/index.scip"), "x").unwrap();
    let filter = WatchFilter::new(&root, &root.join("mydata"));
    assert_eq!(
        filter.relevant(&root.join("index.scip")).as_deref(),
        Some("index.scip")
    );
    assert!(filter.relevant(&root.join("sub/index.scip")).is_none());
    // Directories and paths inside .git never matter.
    assert!(filter.relevant(&root.join(".git/index")).is_none());
    assert!(filter.relevant(&root.join(".git")).is_none());
    assert!(filter.relevant(&root.join("node_modules")).is_none());
    assert!(filter.relevant(&root).is_none());
    // A path outside the root is not ours.
    assert!(
        filter
            .relevant(&root.join("..").join("elsewhere.ts"))
            .is_none()
    );

    // The data dir as the repo root: only its own files are ignored (the database, the
    // server's lock, the usage log that every tool call appends to).
    let filter = WatchFilter::new(&root, &root);
    for f in ["graph.db-wal", "mcp.lock", "stats.jsonl", "stats.1.jsonl"] {
        assert!(filter.relevant(&root.join(f)).is_none(), "{f}");
    }
    assert!(filter.relevant(&root.join("a.ts")).is_some());
    assert!(filter.relevant(&root.join("sub/stats.jsonl")).is_some());
    // Nor are the base snapshots of `diff` / `check --base`, kept in a folder of their own.
    let snapshots = root.join(cartograph_index::git::snapshot::SNAPSHOT_DIR);
    std::fs::create_dir_all(&snapshots).unwrap();
    std::fs::write(snapshots.join("0123abcd.json"), "{}").unwrap();
    assert!(filter.relevant(&snapshots).is_none());
    assert!(filter.relevant(&snapshots.join("0123abcd.json")).is_none());
    assert!(filter.relevant(&root.join("sub/base/x.ts")).is_some());
    // A source folder named as the snapshots' former folder is code.
    assert!(filter.relevant(&root.join("base/x.ts")).is_some());
    // That folder holding only what an older version left there is not (the walker leaves
    // it out too); a file added to it, or a file gone from it, is again.
    let legacy = root.join("legacy");
    let old = "0123456789abcdef0123456789abcdef01234567.json";
    std::fs::create_dir_all(legacy.join("base/t12-3/src")).unwrap();
    std::fs::write(legacy.join("base/t12-3/src/a.ts"), "").unwrap();
    std::fs::write(
        legacy.join("base").join(old),
        r#"{"key":"k","revision_subject":"s","graph":{}}"#,
    )
    .unwrap();
    let filter = WatchFilter::new(&legacy, &legacy);
    let walked = walked_with(&legacy, &legacy);
    assert!(walked.iter().all(|p| !p.starts_with("base/")), "{walked:?}");
    assert!(filter.relevant(&legacy.join("base").join(old)).is_none());
    assert!(
        filter
            .relevant(&legacy.join("base/t12-3/src/a.ts"))
            .is_none()
    );
    assert!(filter.relevant(&legacy.join("base/gone.ts")).is_some());
    std::fs::write(legacy.join("base/x.ts"), "").unwrap();
    assert!(filter.relevant(&legacy.join("base/x.ts")).is_some());
    assert!(filter.relevant(&legacy.join("base").join(old)).is_some());
    let walked = walked_with(&legacy, &legacy);
    assert!(walked.contains("base/x.ts"), "{walked:?}");
    assert!(std::fs::exists(legacy.join("base").join(old)).unwrap());
}

/// Same agreement on the precedence rules: `.ignore` beats `.gitignore` at every level, the
/// git exclude file ranks lowest, and a nested repository behaves as the walker treats it.
#[test]
fn the_filter_follows_the_walker_precedence_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let files = [
        "keep.gen.ts",
        "drop.gen.ts",
        "x.ts",
        "y.ts",
        "nested/a.tmp.ts",
        "nested/b.ts",
        "nested/inner/c.tmp.ts",
        "plain/d.tmp.ts",
        "sub/z.ts",
        "sub/w.ts",
        "sub2/q.ts",
    ];
    for f in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x\n").unwrap();
    }
    // `.ignore` re-includes what `.gitignore` excludes (also one level down).
    std::fs::write(root.join(".gitignore"), "*.gen.ts\n*.tmp.ts\n!x.ts\nw.ts\n").unwrap();
    // A root `.ignore` whitelist beats a deeper `.gitignore`.
    std::fs::write(root.join(".ignore"), "!keep.gen.ts\n!sub2/q.ts\n").unwrap();
    std::fs::write(root.join("sub2/.gitignore"), "q.ts\n").unwrap();
    std::fs::write(root.join("sub/.ignore"), "!w.ts\nz.ts\n").unwrap();
    // The git exclude says ignore x.ts and y.ts; `.gitignore` re-includes x.ts.
    std::fs::create_dir_all(root.join(".git/info")).unwrap();
    std::fs::write(root.join(".git/info/exclude"), "x.ts\ny.ts\n").unwrap();
    // A nested repository; the walker still applies the root's rules inside it.
    std::fs::create_dir_all(root.join("nested/.git")).unwrap();

    let walked = walk(&root, 1 << 20, &[], WalkHooks::default());
    let indexed: BTreeSet<String> = walked.files.iter().map(|f| f.path.clone()).collect();
    // Sanity: the walker really applies these rules.
    assert!(indexed.contains("keep.gen.ts") && !indexed.contains("drop.gen.ts"));
    assert!(indexed.contains("x.ts") && !indexed.contains("y.ts"));
    assert!(indexed.contains("sub2/q.ts"));
    // The walker runs with `require_git(false)`, so a nested `.git` does not stop the
    // root's rules: they still apply inside `nested/`.
    assert!(!indexed.contains("nested/a.tmp.ts") && !indexed.contains("plain/d.tmp.ts"));
    assert!(indexed.contains("nested/b.ts"));

    let filter = WatchFilter::new(&root, &root.join(".cartograph"));
    for f in files {
        let relevant = filter.relevant(&root.join(f)).is_some();
        assert_eq!(relevant, indexed.contains(f), "{f}");
    }
}

/// The files an index of `root` with the data dir `data` reads.
fn walked_with(root: &std::path::Path, data: &std::path::Path) -> BTreeSet<String> {
    let opts = cartograph_index::IndexOptions {
        data_dir: Some(data.to_path_buf()),
        ..Default::default()
    };
    cartograph_index::index(root, &opts)
        .unwrap()
        .graph
        .nodes_of(cartograph_core::NodeKind::File)
        .map(|n| n.path.clone())
        .collect()
}
