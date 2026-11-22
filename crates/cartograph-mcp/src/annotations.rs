//! Notes on nodes written by an agent or a human (`annotate`), shown by `explain`.
//!
//! Stored in `<data dir>/annotations.json`, apart from the graph database (re-indexing
//! never touches them, and no schema migration is needed). Each note keeps the content
//! hash of its node when it was written; `explain` marks it stale once the hash differs.
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
