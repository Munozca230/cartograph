//! A synthetic git repository built at test time with the `git` CLI (tests only; the
//! indexer reads history with gix). Every commit has fixed author and committer dates and
//! made-up people, and git runs with no user or system config, so ids and numbers are the
//! same on every machine.
//!
//! [`history_repo`] builds the shared scenario: churn, three spellings of two people, a
//! rename, a merge, a huge commit, and two files that always change together without any
//! import between them (hidden coupling).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// 2025-01-01T00:00:00Z.
pub const EPOCH: i64 = 1_735_689_600;
pub const DAY: i64 = 86_400;

pub const ADA: (&str, &str) = ("Ada Lovelace", "ada@example.com");
/// Ada again, another email: joined to Ada by name.
pub const ADA_WORK: (&str, &str) = ("Ada Lovelace", "ada@work.example.com");
pub const GRACE: (&str, &str) = ("Grace Hopper", "grace@example.com");
/// Grace again, another spelling: joined to Grace by email.
pub const GRACE_SHORT: (&str, &str) = ("G. Hopper", "grace@example.com");

pub struct GitRepo {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    empty_config: PathBuf,
}

impl GitRepo {
    /// An empty repository on branch `main` (no commit yet).
    pub fn new() -> GitRepo {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let empty_config = tmp.path().join("empty.gitconfig");
        std::fs::write(&empty_config, "").unwrap();
        let r = GitRepo {
            _tmp: tmp,
            root,
            empty_config,
        };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }

    fn command(&self, dir: &Path) -> Command {
        let mut c = Command::new("git");
        c.current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", &self.empty_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
            ]);
        c
    }

    /// Run git in `dir` and return its stdout; panics on failure.
    pub fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.command(dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root, args)
    }

    pub fn write(&self, path: &str, text: &str) {
        let p = self.root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// Append a line to a file.
    pub fn touch(&self, path: &str, line: &str) {
        let p = self.root.join(path);
        let mut text = std::fs::read_to_string(&p).unwrap_or_default();
        text.push_str(line);
        text.push('\n');
        self.write(path, &text);
    }

    /// Stage everything and commit as `who` on day `day` after [`EPOCH`].
    pub fn commit(&self, who: (&str, &str), day: i64, msg: &str) {
        self.git(&["add", "-A"]);
        let date = format!("{} +0000", EPOCH + day * DAY);
        let out = self
            .command(&self.root)
            .env("GIT_AUTHOR_NAME", who.0)
            .env("GIT_AUTHOR_EMAIL", who.1)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_NAME", who.0)
            .env("GIT_COMMITTER_EMAIL", who.1)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["commit", "-q", "--allow-empty", "-m", msg])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Merge `branch` into the current branch with a merge commit, as `who` on `day`.
    pub fn merge(&self, branch: &str, who: (&str, &str), day: i64) {
        let date = format!("{} +0000", EPOCH + day * DAY);
        let out = self
            .command(&self.root)
            .env("GIT_AUTHOR_NAME", who.0)
            .env("GIT_AUTHOR_EMAIL", who.1)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_NAME", who.0)
            .env("GIT_COMMITTER_EMAIL", who.1)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["merge", "-q", "--no-ff", "-m", "merge", branch])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "merge failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `git log` count of non-merge commits touching `path` (renames followed with
    /// `follow`), the reference the indexer must match.
    pub fn log_count(&self, path: &str, follow: bool) -> usize {
        let mut args = vec!["log", "--no-merges", "--format=%H"];
        if follow {
            args.push("--follow");
        } else {
            args.push("--full-history");
        }
        args.extend(["--", path]);
        self.git(&args).lines().filter(|l| !l.is_empty()).count()
    }
}

/// The shared scenario. Files at HEAD:
/// - `src/client.ts` and `src/server.ts` change together in 10 commits with no import
///   between them: hidden coupling;
/// - `src/api.ts` imports `./db` (a static dependency) and changes with it as often: no
///   finding;
/// - `src/helpers.ts` was `src/util.ts` (renamed with an edit on day 30);
/// - `gen/f00.txt` … `gen/f59.txt` come in with one huge commit that also touches
///   `client.ts` and `server.ts` (left out of co-change);
/// - a `feature` branch edits `db.ts` and `api.ts` and is merged with `--no-ff`;
/// - only `server.ts` changes on day 200, the only commit in the 90-day window.
///
/// People: Ada writes with two emails (joined by name), Grace with two spellings (joined
/// by email).
pub fn history_repo() -> GitRepo {
    let r = GitRepo::new();
    r.write(
        "package.json",
        "{ \"name\": \"fixture\" }
",
    );
    r.write(
        "src/db.ts",
        "export const db = 1;
",
    );
    r.write(
        "src/api.ts",
        "import { db } from \"./db\";
export function api() {
    return db;
}
",
    );
    r.write(
        "src/util.ts",
        "export function util() {
    if (true) {
        return 1;
    }
}
",
    );
    r.write(
        "src/client.ts",
        "export const channel = \"orders\";
",
    );
    r.write(
        "src/server.ts",
        "export const route = \"orders\";
",
    );
    r.commit(ADA, 0, "initial");
    for i in 0..8i64 {
        let who = [ADA, GRACE, ADA_WORK, GRACE_SHORT][(i % 4) as usize];
        r.touch("src/client.ts", &format!("// client change {i}"));
        r.touch("src/server.ts", &format!("// server change {i}"));
        r.commit(who, 2 + 3 * i, &format!("orders protocol {i}"));
        r.touch("src/api.ts", &format!("// api {i}"));
        r.touch("src/db.ts", &format!("// db {i}"));
        r.commit(
            [GRACE, ADA][(i % 2) as usize],
            3 + 3 * i,
            &format!("storage {i}"),
        );
    }
    // Rename with an edit.
    r.git(&["mv", "src/util.ts", "src/helpers.ts"]);
    r.touch("src/helpers.ts", "// renamed");
    r.commit(GRACE, 30, "rename util to helpers");
    r.touch("src/helpers.ts", "// after rename");
    r.commit(ADA, 32, "helpers tweak");
    // A huge commit: 60 generated files plus the coupled pair.
    for i in 0..60 {
        r.write(&format!("gen/f{i:02}.txt"), &format!("generated {i}\n"));
    }
    r.touch("src/client.ts", "// regenerated");
    r.touch("src/server.ts", "// regenerated");
    r.commit(GRACE, 40, "regenerate everything");
    // A branch merged with a merge commit.
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.touch("src/db.ts", "// feature db");
    r.touch("src/api.ts", "// feature api");
    r.commit(GRACE, 50, "feature work");
    r.git(&["checkout", "-q", "main"]);
    r.touch("src/client.ts", "// main client");
    r.touch("src/server.ts", "// main server");
    r.commit(ADA, 51, "orders protocol 5");
    r.merge("feature", ADA, 52);
    // Much later: only the server changes (recent window of 90 days ends here).
    r.touch("src/server.ts", "// late");
    r.commit(GRACE, 200, "late server fix");
    r
}
