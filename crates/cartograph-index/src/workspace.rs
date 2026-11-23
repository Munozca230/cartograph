//! `cartograph workspace init`: a `cartograph-workspace.toml` proposed from the folders of
//! a parent folder. A member is a child folder that is a git repository (a `.git` folder,
//! or the `.git` file of a linked worktree) and holds code; worktree copies of a sibling
//! (`<repo>-worktrees`), folders named like output or data (logs, backups…) and folders
//! without code are left out, each with why.

use crate::walk::EXCLUDED_DIRS;
use std::path::Path;

/// A proposed member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    pub name: String,
    /// The folder, relative to the parent.
    pub path: String,
}

/// A child folder left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub path: String,
    pub why: String,
}

/// Code files looked for before a folder counts as codeless.
const CODE_PROBE: usize = 1;
/// How deep the probe looks.
const PROBE_DEPTH: usize = 6;

/// The members proposed for `dir` and the child folders left out, both sorted by folder.
pub fn propose(dir: &Path) -> std::io::Result<(Vec<Proposed>, Vec<LeftOut>)> {
    let mut children: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    children.sort();
    let mut members: Vec<Proposed> = Vec::new();
    let mut left = Vec::new();
    for c in children {
        let path = dir.join(&c);
        let skip = |why: &str| LeftOut {
            path: c.clone(),
            why: why.to_string(),
        };
        if c.starts_with('.') || EXCLUDED_DIRS.contains(&c.as_str()) {
            continue;
        }
        if let Some(why) = crate::noise::sibling_worktrees(&path, Some(&c)) {
            left.push(skip(&why));
            continue;
        }
        if let Some((why, _)) = crate::noise::by_name(&c) {
            left.push(skip(why));
            continue;
        }
        if !path.join(".git").exists() {
            left.push(skip("not a git repository"));
            continue;
        }
        if !has_code(&path) {
            left.push(skip("no code"));
            continue;
        }
        let name = member_name(&c, &members);
        members.push(Proposed { name, path: c });
    }
    Ok((members, left))
}

/// Whether `dir` holds a code file the walk would index (its ignore rules applied).
fn has_code(dir: &Path) -> bool {
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .parents(false)
        .require_git(false)
        .git_global(false)
        .max_depth(Some(PROBE_DEPTH))
        .filter_entry(|e| {
            !e.file_name()
                .to_str()
                .is_some_and(|n| EXCLUDED_DIRS.contains(&n))
        })
        .build();
    walker
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| crate::lang::detect(n).is_some())
        })
        .take(CODE_PROBE)
        .count()
        >= CODE_PROBE
}

/// A valid member name for the folder `folder`, distinct from those of `taken`: lowercase,
/// other characters as `-`, a letter first; a reserved or too short name gets `-repo`.
pub fn member_name(folder: &str, taken: &[Proposed]) -> String {
    let mut s: String = folder
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let mut s = s.trim_matches('-').to_string();
    if !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        s = format!("m-{s}").trim_end_matches('-').to_string();
    }
    if cartograph_core::workspace::valid_member_name(&s).is_err() {
        s = format!("{s}-repo");
    }
    let base = s.clone();
    let mut n = 2;
    while taken.iter().any(|p| p.name == s) {
        s = format!("{base}-{n}");
        n += 1;
    }
    s
}

/// The text of the proposed `cartograph-workspace.toml`.
pub fn render(name: &str, members: &[Proposed], left: &[LeftOut]) -> String {
    let mut s = String::from(
        "# A workspace: several repositories mapped as one (see docs/workspace.md).\n\
         # Each member is a top-level folder of the map, named after the member.\n",
    );
    if !left.is_empty() {
        s.push_str("# Folders left out:\n");
        for l in left {
            s.push_str(&format!("#   {}: {}\n", l.path, l.why));
        }
    }
    s.push_str(&format!("\n[workspace]\nname = {}\n", quote(name)));
    for m in members {
        s.push_str(&format!(
            "\n[[member]]\nname = {}\npath = {}\n",
            quote(&m.name),
            quote(&m.path)
        ));
    }
    s
}

/// A TOML basic string.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_valid_and_distinct() {
        let none: Vec<Proposed> = Vec::new();
        assert_eq!(member_name("Backend_API", &none), "backend-api");
        assert_eq!(member_name("2fa", &none), "m-2fa");
        assert_eq!(member_name("go", &none), "go-repo");
        assert_eq!(member_name("x", &none), "x-repo");
        let taken = vec![Proposed {
            name: "web".into(),
            path: "Web".into(),
        }];
        assert_eq!(member_name("web", &taken), "web-2");
    }

    #[test]
    fn children_with_git_and_code_are_proposed() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = |p: &str, text: &str| {
            let f = tmp.path().join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        };
        mk("Api/.git/HEAD", "ref: refs/heads/main\n");
        mk("Api/src/Main.java", "class Main {}\n");
        mk("Api-worktrees/feat/.git", "gitdir: x\n");
        mk("Api-worktrees/feat/src/Main.java", "class Main {}\n");
        mk("web/.git", "gitdir: ../somewhere\n");
        mk("web/src/index.ts", "export {};\n");
        mk("notes/.git/HEAD", "ref: refs/heads/main\n");
        mk("notes/README.md", "# notes\n");
        mk("logs/.git/HEAD", "x\n");
        mk("loose/a.ts", "export {};\n");
        let (members, left) = propose(tmp.path()).unwrap();
        assert_eq!(
            members,
            vec![
                Proposed {
                    name: "api".into(),
                    path: "Api".into()
                },
                Proposed {
                    name: "web".into(),
                    path: "web".into()
                },
            ]
        );
        let why = |p: &str| left.iter().find(|l| l.path == p).map(|l| l.why.clone());
        assert!(why("Api-worktrees").unwrap().contains("worktrees"));
        assert_eq!(why("notes").as_deref(), Some("no code"));
        assert!(why("logs").is_some());
        assert_eq!(why("loose").as_deref(), Some("not a git repository"));
        let text = render("parent", &members, &left);
        let ws = cartograph_core::Workspace::parse(&text, tmp.path(), "x").unwrap();
        assert_eq!(ws.names(), vec!["api", "web"]);
    }
}
