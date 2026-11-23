//! Workspaces: several repositories mapped as one, declared by a
//! `cartograph-workspace.toml` in the folder cartograph is pointed at (never looked for
//! above it, so a member folder alone is still a plain repo).
//!
//! Each member is a top-level folder of the graph named after the member, whatever its
//! folder on disk is called or wherever it is: `file:api/src/a.ts` is `src/a.ts` of the
//! member `api`. `api:src/a.ts` is an alias of that path, accepted wherever a path is
//! ([`expand_alias`]); outputs always print the canonical form.
//!
//! This module reads the members. The architecture rules of the file (`[[layers]]`,
//! `[rules]`, `ignore`, `[thresholds]`, `[findings]`) are read by `cartograph-query` with
//! the grammar of `cartograph.toml`; `[bridges]` is read here; `[member.http]` and
//! `produces` are accepted and left to the features that read them.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use toml::Spanned;

/// File name of a workspace, in the folder it maps.
pub const WORKSPACE_FILE: &str = "cartograph-workspace.toml";

/// Names a member cannot have: id kinds, ecosystems of external ids and bridge words, so
/// `name:` never reads as anything but a member.
pub const RESERVED: [&str; 28] = [
    "file", "dir", "sym", "repo", "package", "pkg", "external", "ext", "npm", "node", "jsr", "bun",
    "pypi", "python", "maven", "jdk", "gomod", "go", "nuget", "dotnet", "cargo", "godot", "res",
    "http", "https", "ipc", "bridge", "uid",
];

/// One repository of a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// Its folder in the graph.
    pub name: String,
    /// Its folder as the workspace file writes it (relative to the file, or absolute).
    pub path: String,
    /// Its folder on disk: absolute, `.` and `..` folded (canonical when it exists).
    pub root: PathBuf,
}

/// A parsed `cartograph-workspace.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The file.
    pub file: PathBuf,
    /// The folder holding it, which the workspace maps.
    pub dir: PathBuf,
    /// `[workspace] name`, else the folder's name.
    pub name: String,
    /// In the order of the file.
    pub members: Vec<Member>,
    /// `[bridges]` as written (see [`crate::bridge::BridgeSettings`]).
    pub bridges: crate::bridge::BridgeSettings,
}

/// A problem in a workspace file: where (file, line, key) and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceError {
    pub origin: String,
    pub line: Option<usize>,
    pub key: Option<String>,
    pub message: String,
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.origin)?;
        if let Some(l) = self.line {
            write!(f, ":{l}")?;
        }
        write!(f, ": ")?;
        if let Some(k) = &self.key {
            write!(f, "{k}: ")?;
        }
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WorkspaceError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    workspace: Option<RawWorkspace>,
    #[serde(default)]
    member: Vec<Spanned<RawMember>>,
    // Read by cartograph-query (the grammar of `cartograph.toml`).
    #[serde(default)]
    layers: Option<toml::Value>,
    #[serde(default)]
    rules: Option<toml::Value>,
    #[serde(default)]
    ignore: Option<toml::Value>,
    #[serde(default)]
    thresholds: Option<toml::Value>,
    #[serde(default)]
    findings: Option<toml::Value>,
    // Cross-repository links (HTTP, IPC, packages): read by the features that build them.
    #[serde(default)]
    bridges: Option<Spanned<toml::Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkspace {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMember {
    name: Spanned<String>,
    path: Spanned<String>,
    #[serde(default)]
    http: Option<toml::Value>,
    #[serde(default)]
    produces: Option<toml::Value>,
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Whether `name` can name a member: `[a-z][a-z0-9-]+` and not [`RESERVED`].
pub fn valid_member_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let shape = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() >= 2
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !shape {
        return Err(format!(
            "{name:?} is not a member name: lowercase letters, digits and `-`, starting with a letter, at least 2 characters"
        ));
    }
    if RESERVED.contains(&name) {
        return Err(format!(
            "{name:?} is reserved (it reads as an id kind or an ecosystem)"
        ));
    }
    Ok(())
}

impl Workspace {
    /// The workspace of `dir`: its `cartograph-workspace.toml` parsed, `None` without one.
    pub fn find(dir: &Path) -> Result<Option<Workspace>, WorkspaceError> {
        let file = dir.join(WORKSPACE_FILE);
        if !file.is_file() {
            return Ok(None);
        }
        Workspace::load(&file).map(Some)
    }

    /// Read and validate a workspace file.
    pub fn load(file: &Path) -> Result<Workspace, WorkspaceError> {
        let origin = file.display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(file).map_err(|e| WorkspaceError {
            origin: origin.clone(),
            line: None,
            key: None,
            message: format!("cannot read: {e}"),
        })?;
        let abs = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
        let dir = clean(abs.parent().unwrap_or(Path::new(".")));
        Workspace::parse(&text, &dir, &origin).map(|mut w| {
            w.file = clean(&abs);
            w
        })
    }

    /// Parse workspace text whose relative member paths start at `dir`.
    pub fn parse(text: &str, dir: &Path, origin: &str) -> Result<Workspace, WorkspaceError> {
        let err = |line: Option<usize>, key: Option<String>, message: String| WorkspaceError {
            origin: origin.to_string(),
            line,
            key,
            message,
        };
        let raw: RawFile = toml::from_str(text).map_err(|e| {
            let line = e.span().map(|s| line_of(text, s.start));
            let msg = e.message().lines().next().unwrap_or("invalid TOML");
            err(line, None, msg.to_string())
        })?;
        let at = |s: std::ops::Range<usize>| Some(line_of(text, s.start));
        if raw.member.is_empty() {
            return Err(err(
                None,
                Some("member".into()),
                "a workspace needs at least one [[member]]".into(),
            ));
        }
        let mut members: Vec<Member> = Vec::new();
        for (i, m) in raw.member.iter().enumerate() {
            let span = m.span();
            let m = m.get_ref();
            let name = m.name.get_ref().trim().to_string();
            valid_member_name(&name)
                .map_err(|e| err(at(m.name.span()), Some(format!("member[{i}].name")), e))?;
            if members.iter().any(|x| x.name == name) {
                return Err(err(
                    at(m.name.span()),
                    Some(format!("member[{i}].name")),
                    format!("duplicate member {name:?}"),
                ));
            }
            let path = m.path.get_ref().trim().to_string();
            if path.is_empty() {
                return Err(err(
                    at(m.path.span()),
                    Some(format!("member[{i}].path")),
                    "empty path".into(),
                ));
            }
            let root = resolve_root(dir, &path);
            if let Some(other) = members
                .iter()
                .find(|x| root.starts_with(&x.root) || x.root.starts_with(&root))
            {
                return Err(err(
                    at(span),
                    Some(format!("member[{i}].path")),
                    format!(
                        "{path:?} overlaps member {:?} ({}): members are separate folders",
                        other.name, other.path
                    ),
                ));
            }
            let _ = (&m.http, &m.produces);
            members.push(Member { name, path, root });
        }
        let _ = (
            &raw.layers,
            &raw.rules,
            &raw.ignore,
            &raw.thresholds,
            &raw.findings,
        );
        let bridges = match &raw.bridges {
            Some(b) => crate::bridge::BridgeSettings::parse(b.get_ref())
                .map_err(|m| err(at(b.span()), Some("bridges".into()), m))?,
            None => crate::bridge::BridgeSettings::default(),
        };
        let name = raw
            .workspace
            .and_then(|w| w.name)
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                dir.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("workspace")
                    .to_string()
            });
        Ok(Workspace {
            file: dir.join(WORKSPACE_FILE),
            dir: dir.to_path_buf(),
            name,
            members,
            bridges,
        })
    }

    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    /// Names of the members, in the order of the file.
    pub fn names(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.name.as_str()).collect()
    }

    /// Where a path of the graph (`api/src/a.ts`) is on disk; `None` outside every member
    /// (the workspace's own top level holds nothing but members).
    pub fn to_disk(&self, path: &str) -> Option<PathBuf> {
        let (first, rest) = path.split_once('/').unwrap_or((path, ""));
        let m = self.member(first)?;
        Some(if rest.is_empty() {
            m.root.clone()
        } else {
            m.root.join(rest)
        })
    }

    /// The path of the graph of a place on disk: relative to the member whose folder holds
    /// it (`None` outside every member).
    pub fn from_disk(&self, abs: &Path) -> Option<String> {
        let abs = clean(&std::path::absolute(abs).ok()?);
        self.members.iter().find_map(|m| {
            let rest = abs.strip_prefix(&m.root).ok()?;
            let rel: Vec<&str> = rest
                .components()
                .map(|c| c.as_os_str().to_str())
                .collect::<Option<_>>()?;
            Some(if rel.is_empty() {
                m.name.clone()
            } else {
                format!("{}/{}", m.name, rel.join("/"))
            })
        })
    }
}

/// `path` of the workspace file as an absolute folder: relative to `dir`, `.`/`..`
/// folded, canonical (without Windows' `\\?\`) when it exists.
fn resolve_root(dir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    };
    let folded = clean(&joined);
    match std::fs::canonicalize(&folded) {
        Ok(c) => clean(&strip_verbatim(c)),
        Err(_) => folded,
    }
}

/// `p` with `.` and `..` folded, without Windows' `\\?\` prefix.
pub fn clean(p: &Path) -> PathBuf {
    let p = strip_verbatim(p.to_path_buf());
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

/// Id kinds a path can come with (`file:api:src/a.ts`).
const KINDS: [&str; 3] = ["file:", "dir:", "sym:"];

/// `input` with a member alias spelled as the canonical path: `api:src/a.ts` →
/// `api/src/a.ts`, `file:api:src/a.ts` → `file:api/src/a.ts`, `sym:api:src/a.ts#f` →
/// `sym:api/src/a.ts#f`, `api:` → `api`. `None` when `input` names no member of
/// `members` that way (a plain path, an external id such as `npm:react`).
pub fn expand_alias<S: AsRef<str>>(input: &str, members: &[S]) -> Option<String> {
    let t = input.trim();
    let (kind, rest) = KINDS
        .iter()
        .find_map(|k| t.strip_prefix(k).map(|r| (*k, r)))
        .unwrap_or(("", t));
    let (name, path) = rest.split_once(':')?;
    if !members.iter().any(|m| m.as_ref() == name) {
        return None;
    }
    let path = path.replace('\\', "/");
    let path = path.trim_start_matches("./").trim_start_matches('/');
    Some(if path.is_empty() || path == "." {
        format!("{kind}{name}")
    } else {
        format!("{kind}{name}/{path}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Workspace, WorkspaceError> {
        Workspace::parse(text, Path::new("/ws"), "cartograph-workspace.toml")
    }

    #[test]
    fn members_parse_with_their_folders_and_v4_tables_are_accepted() {
        let w = parse(
            r#"
[workspace]
name = "system"
[[member]]
name = "api"
path = "backend-api"
[member.http]
context_path = "/api"
[[member]]
name = "lib"
path = "../shared-lib"
produces = ["maven:com.example:lib"]
[bridges]
http = true
[[layers]]
name = "lib"
paths = ["lib"]
"#,
        )
        .unwrap();
        assert_eq!(w.name, "system");
        assert_eq!(w.names(), vec!["api", "lib"]);
        assert_eq!(w.members[0].root, clean(Path::new("/ws/backend-api")));
        assert_eq!(w.members[1].root, clean(Path::new("/shared-lib")));
        assert_eq!(
            w.to_disk("api/src/a.ts"),
            Some(clean(Path::new("/ws/backend-api")).join("src/a.ts"))
        );
        assert_eq!(w.to_disk("api"), Some(clean(Path::new("/ws/backend-api"))));
        assert_eq!(w.to_disk("logs/app.log"), None);
        assert_eq!(w.bridges.http, Some(true));
        assert_eq!(w.bridges.ipc, None);
        let bad = parse("[[member]]\nname = \"api\"\npath = \"a\"\n[bridges]\nhttps = true\n")
            .unwrap_err();
        assert_eq!(bad.key.as_deref(), Some("bridges"));
        assert_eq!(bad.line, Some(4));
    }

    #[test]
    fn bad_members_are_errors_with_their_line() {
        let e = parse("[[member]]\nname = \"npm\"\npath = \"x\"\n").unwrap_err();
        assert_eq!(e.line, Some(2));
        assert!(e.to_string().contains("reserved"), "{e}");
        let e = parse("[[member]]\nname = \"A\"\npath = \"x\"\n").unwrap_err();
        assert!(e.to_string().contains("member[0].name"), "{e}");
        let e = parse(
            "[[member]]\nname = \"ab\"\npath = \"x\"\n[[member]]\nname = \"ab\"\npath = \"y\"\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("duplicate"), "{e}");
        let e = parse(
            "[[member]]\nname = \"ab\"\npath = \"x\"\n[[member]]\nname = \"cd\"\npath = \"x/y\"\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("overlaps"), "{e}");
        let e = parse("[workspace]\nname = \"x\"\n").unwrap_err();
        assert!(e.to_string().contains("at least one"), "{e}");
        let e = parse("[[member]]\nname = \"ab\"\npath = \"x\"\ntypo = 1\n").unwrap_err();
        assert!(e.to_string().contains("typo"), "{e}");
    }

    #[test]
    fn aliases_expand_to_canonical_paths() {
        let m = ["api", "desktop"];
        assert_eq!(
            expand_alias("api:src/a.ts", &m).as_deref(),
            Some("api/src/a.ts")
        );
        assert_eq!(
            expand_alias("file:api:src/a.ts", &m).as_deref(),
            Some("file:api/src/a.ts")
        );
        assert_eq!(
            expand_alias("sym:desktop:src/a.ts#f", &m).as_deref(),
            Some("sym:desktop/src/a.ts#f")
        );
        assert_eq!(expand_alias("dir:api:", &m).as_deref(), Some("dir:api"));
        assert_eq!(expand_alias("api:.\\src", &m).as_deref(), Some("api/src"));
        assert_eq!(expand_alias("npm:react", &m), None);
        assert_eq!(expand_alias("src/a.ts", &m), None);
        assert_eq!(expand_alias("lib:x", &m), None);
    }

    #[test]
    fn member_names() {
        assert!(valid_member_name("api").is_ok());
        assert!(valid_member_name("web-2").is_ok());
        for bad in ["a", "2a", "Api", "a_b", "file", "npm", ""] {
            assert!(valid_member_name(bad).is_err(), "{bad}");
        }
    }
}
