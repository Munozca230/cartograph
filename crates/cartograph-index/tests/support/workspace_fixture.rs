//! `tests/fixtures/workspace-basic` with its members made git repositories at test time
//! (git cannot commit a `.git` inside another repository's tree): a copy in a temp folder,
//! one repository per member with fixed dates and people, and the `.git` file of the
//! linked worktree copy `api-worktrees/feat` (never walked: not a member).

#![allow(dead_code)]

use crate::git_fixture::{ADA, GRACE, GitRepo};
use std::path::{Path, PathBuf};

/// Member names and their folders, in the order of the workspace file.
pub const MEMBERS: [(&str, &str); 4] = [
    ("lib", "shared-lib"),
    ("api", "backend-api"),
    ("desktop", "desktop-app"),
    ("mcp", "mcp"),
];

pub struct Workspace {
    _tmp: tempfile::TempDir,
    /// The folder holding `cartograph-workspace.toml`.
    pub root: PathBuf,
    /// One repository per member, in [`MEMBERS`] order.
    pub repos: Vec<GitRepo>,
}

impl Workspace {
    pub fn repo(&self, member: &str) -> &GitRepo {
        let i = MEMBERS.iter().position(|(m, _)| *m == member).unwrap();
        &self.repos[i]
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            std::fs::copy(entry.path(), &dst).unwrap();
        }
    }
}

/// The workspace: every member committed once on day 1 (by Ada), and the API a second
/// time on day 3 (by Grace, the controller and the service together).
pub fn workspace_basic() -> Workspace {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace-basic");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/workspace-basic");
    copy_dir(&fixture, &root);
    std::fs::write(
        root.join("api-worktrees/feat/.git"),
        "gitdir: ../../backend-api/.git/worktrees/feat\n",
    )
    .unwrap();
    let repos: Vec<GitRepo> = MEMBERS
        .iter()
        .map(|(_, folder)| {
            let r = GitRepo::init_at(&root.join(folder));
            r.commit(ADA, 1, "first");
            r
        })
        .collect();
    let api = &repos[1];
    api.touch(
        "src/main/java/com/acme/api/UserController.java",
        "// reviewed",
    );
    api.touch("src/main/java/com/acme/api/UserService.java", "// reviewed");
    api.commit(GRACE, 3, "review");
    Workspace {
        _tmp: tmp,
        root,
        repos,
    }
}
