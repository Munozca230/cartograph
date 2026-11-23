//! Notes on nodes written by an agent or a human (`annotate`), shown by `explain`.
//!
//! Two places hold them, apart from the graph database (re-indexing never touches them,
//! and no schema migration is needed):
//! - `<repo>/cartograph-notes.toml` ([`REPO_FILE`]): versioned with the code, shared by
//!   everyone who clones it, one TOML table per node (readable, and one changed note is a
//!   small diff). `annotate` writes here when the file exists or when asked to
//!   (`cartograph mcp --notes-in-repo`). The walk and the watcher leave it out, so writing
//!   a note never changes a hash.
//! - `<data dir>/annotations.json` ([`FILE`]): local, the default.
//!
//! `explain` reads both; a node noted in both shows the repo's note. Each note keeps the
//! content hash of its node when it was written; `explain` marks it stale once the hash
//! differs.
//! A file's hash is its BLAKE3 digest from the index; a folder's is a BLAKE3 over the
//! sorted `(path, hash)` of every file under it, so adding, removing or editing any file
//! inside makes the folder's note stale.

use cartograph_query::explain::Annotation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// File name inside the data dir.
pub const FILE: &str = "annotations.json";
/// Longest note accepted, in characters (keeps `explain` inside its budget).
pub const MAX_CHARS: usize = 600;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub text: String,
    /// Content hash of the node when the note was written.
    pub hash: String,
    /// Seconds since the Unix epoch.
    pub updated: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notes {
    #[serde(default = "version")]
    pub version: u32,
    /// By stable node id (`file:src/a.rs`, `dir:src`).
    #[serde(default)]
    pub notes: BTreeMap<String, Note>,
}

fn version() -> u32 {
    1
}

/// The notes file at the repo root.
pub const REPO_FILE: &str = cartograph_index::NOTES_FILE;

/// First lines of [`REPO_FILE`].
const REPO_HEADER: &str =
    "# Notes on files and folders, shown by `cartograph explain` (and the MCP `explain` tool).
# Written by the `annotate` tool; edit them by hand as well. One table per node id.
# `hash` is the content hash of the node when the note was written: when the code
# changes, explain shows the note as stale until it is written again.
";

/// A note as [`REPO_FILE`] keeps it: no timestamp (git has it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RepoNote {
    text: String,
    hash: String,
}

/// Read `<root>/cartograph-notes.toml`; a missing file is an empty set, a file that does
/// not parse is an error (never overwritten silently).
pub fn load_repo(root: &Path) -> Result<Notes, String> {
    let p = root.join(REPO_FILE);
    let text = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Notes {
                version: 1,
                ..Notes::default()
            });
        }
        Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
    };
    let raw: BTreeMap<String, RepoNote> =
        toml::from_str(&text).map_err(|e| format!("{} is not valid: {e}", p.display()))?;
    Ok(Notes {
        version: 1,
        notes: raw
            .into_iter()
            .map(|(id, n)| {
                (
                    id,
                    Note {
                        text: n.text,
                        hash: n.hash,
                        updated: 0,
                    },
                )
            })
            .collect(),
    })
}

/// Write `<root>/cartograph-notes.toml` atomically (a temporary file next to it, then a
/// rename), sorted by node id.
pub fn save_repo(root: &Path, notes: &Notes) -> Result<(), String> {
    let raw: BTreeMap<&str, RepoNote> = notes
        .notes
        .iter()
        .map(|(id, n)| {
            (
                id.as_str(),
                RepoNote {
                    text: n.text.clone(),
                    hash: n.hash.clone(),
                },
            )
        })
        .collect();
    let body = toml::to_string(&raw).map_err(|e| e.to_string())?;
    let p = root.join(REPO_FILE);
    let tmp = root.join(format!("{REPO_FILE}.tmp"));
    std::fs::write(&tmp, format!("{REPO_HEADER}\n{body}"))
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Whether the repo keeps its notes in [`REPO_FILE`] (the file exists).
pub fn repo_has_notes(root: &Path) -> bool {
    root.join(REPO_FILE).is_file()
}

/// The notes `explain` shows: those of the data dir, then those of the repo over them.
/// A file that cannot be read or parsed is left out with a warning in the result's
/// second part (the other one still counts).
pub fn load_all(data_dir: &Path, root: &Path) -> (Notes, Vec<String>) {
    let mut warnings = Vec::new();
    let mut all = load(data_dir).unwrap_or_else(|e| {
        warnings.push(e);
        Notes::default()
    });
    match load_repo(root) {
        Ok(repo) => all.notes.extend(repo.notes),
        Err(e) => warnings.push(e),
    }
    (all, warnings)
}

/// Path of the notes file of a data dir.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// Read the notes; a missing file is an empty set. A file that cannot be parsed is an
/// error (it is never overwritten silently).
pub fn load(data_dir: &Path) -> Result<Notes, String> {
    let p = path(data_dir);
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{} is not valid: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Notes {
            version: 1,
            ..Notes::default()
        }),
        Err(e) => Err(format!("cannot read {}: {e}", p.display())),
    }
}

/// Write atomically (temp file + rename).
pub fn save(data_dir: &Path, notes: &Notes) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
    let p = path(data_dir);
    let tmp = data_dir.join(format!("{FILE}.tmp"));
    let body = serde_json::to_string_pretty(notes).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Content hashes of the indexed files, by repo-relative path.
pub type FileHashes = BTreeMap<String, String>;

/// Hashes of every file node of a graph.
pub fn file_hashes(graph: &cartograph_core::Graph) -> FileHashes {
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == cartograph_core::NodeKind::File)
        .map(|n| (n.path.clone(), n.hash.clone().unwrap_or_default()))
        .collect()
}

/// Content hash of a node id (`file:…`, `dir:…`, `repo:`), or `None` for nodes without
/// content (libraries) or ids not in the index.
pub fn content_hash(files: &FileHashes, id: &str) -> Option<String> {
    if let Some(p) = id.strip_prefix("file:") {
        return files.get(p).cloned();
    }
    let dir = if let Some(d) = id.strip_prefix("dir:") {
        d
    } else if id == "repo:" {
        ""
    } else {
        return None;
    };
    let mut h = blake3::Hasher::new();
    let mut any = false;
    let prefix = format!("{dir}/");
    let range: Box<dyn Iterator<Item = (&String, &String)>> = if dir.is_empty() {
        Box::new(files.iter())
    } else {
        Box::new(
            files
                .range(prefix.clone()..)
                .take_while(|(p, _)| p.starts_with(&prefix)),
        )
    };
    for (p, fh) in range {
        any = true;
        h.update(p.as_bytes());
        h.update(&[0]);
        h.update(fh.as_bytes());
        h.update(b"\n");
    }
    any.then(|| h.finalize().to_hex().to_string())
}

/// The note of `id` for `explain`, stale when the content hash changed.
pub fn annotation(notes: &Notes, files: &FileHashes, id: &str) -> Option<Annotation> {
    let n = notes.notes.get(id)?;
    let now = content_hash(files, id);
    Some(Annotation {
        text: n.text.clone(),
        stale: now.as_deref() != Some(n.hash.as_str()),
    })
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> FileHashes {
        [
            ("src/a.rs", "h1"),
            ("src/b.rs", "h2"),
            ("src2/c.rs", "h3"),
            ("x.rs", "h4"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
    }

    #[test]
    fn hashes_of_files_dirs_and_repo() {
        let f = files();
        assert_eq!(content_hash(&f, "file:src/a.rs").as_deref(), Some("h1"));
        assert_eq!(content_hash(&f, "file:nope.rs"), None);
        let src = content_hash(&f, "dir:src").unwrap();
        // `src2/` is not inside `src/`.
        let mut g = f.clone();
        g.insert("src2/c.rs".into(), "changed".into());
        assert_eq!(content_hash(&g, "dir:src").unwrap(), src);
        g.insert("src/new.rs".into(), "h9".into());
        assert_ne!(content_hash(&g, "dir:src").unwrap(), src);
        assert!(content_hash(&f, "repo:").is_some());
        assert_eq!(content_hash(&f, "dir:empty"), None);
        assert_eq!(content_hash(&f, "external:npm:react"), None);
    }

    #[test]
    fn stale_when_the_content_changes() {
        let f = files();
        let mut notes = Notes::default();
        notes.notes.insert(
            "file:src/a.rs".into(),
            Note {
                text: "parses things".into(),
                hash: "h1".into(),
                updated: 0,
            },
        );
        let a = annotation(&notes, &f, "file:src/a.rs").unwrap();
        assert!(!a.stale);
        let mut g = f.clone();
        g.insert("src/a.rs".into(), "h1b".into());
        assert!(annotation(&notes, &g, "file:src/a.rs").unwrap().stale);
        assert!(annotation(&notes, &f, "file:src/b.rs").is_none());
    }

    #[test]
    fn repo_file_round_trip_and_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, data) = (tmp.path().join("r"), tmp.path().join("d"));
        std::fs::create_dir_all(&root).unwrap();
        assert!(!repo_has_notes(&root));
        assert!(load_repo(&root).unwrap().notes.is_empty());
        let note = |text: &str| Note {
            text: text.into(),
            hash: "h".into(),
            updated: 7,
        };
        let mut repo = Notes::default();
        repo.notes
            .insert("dir:src".into(), note("shared\nsecond line"));
        repo.notes
            .insert("file:src/a.rs".into(), note("from the repo"));
        save_repo(&root, &repo).unwrap();
        assert!(repo_has_notes(&root));
        assert!(!root.join(format!("{REPO_FILE}.tmp")).exists());
        let text = std::fs::read_to_string(root.join(REPO_FILE)).unwrap();
        assert!(text.starts_with("# Notes on files and folders"), "{text}");
        assert!(!text.contains("updated"), "{text}");
        let back = load_repo(&root).unwrap();
        assert_eq!(back.notes["dir:src"].text, "shared\nsecond line");
        assert_eq!(back.notes["file:src/a.rs"].hash, "h");
        // The repo's note wins over the data dir's for the same node.
        let mut local = Notes::default();
        local.notes.insert("file:src/a.rs".into(), note("local"));
        local
            .notes
            .insert("file:src/b.rs".into(), note("only local"));
        save(&data, &local).unwrap();
        let (all, warnings) = load_all(&data, &root);
        assert!(warnings.is_empty());
        assert_eq!(all.notes["file:src/a.rs"].text, "from the repo");
        assert_eq!(all.notes["file:src/b.rs"].text, "only local");
        assert_eq!(all.notes.len(), 3);
        // A broken repo file is reported, the local notes still count.
        std::fs::write(root.join(REPO_FILE), "not = [toml").unwrap();
        let (all, warnings) = load_all(&data, &root);
        assert_eq!(warnings.len(), 1);
        assert_eq!(all.notes.len(), 2);
    }

    #[test]
    fn save_load_round_trip_and_bad_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load(tmp.path()).unwrap().notes.is_empty());
        let mut n = Notes::default();
        n.notes.insert(
            "dir:src".into(),
            Note {
                text: "core".into(),
                hash: "x".into(),
                updated: 1,
            },
        );
        save(tmp.path(), &n).unwrap();
        assert_eq!(load(tmp.path()).unwrap().notes, n.notes);
        std::fs::write(path(tmp.path()), "{ not json").unwrap();
        assert!(load(tmp.path()).is_err());
    }
}
