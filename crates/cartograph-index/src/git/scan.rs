//! Reading history with gix: open the repository around the indexed root, walk the commits
//! reachable from HEAD (all parents, merges included) and diff each non-merge commit with
//! its first parent, renames detected as `git log` does (50 % similarity, no copies).
//! Diffs run in parallel, one thread-local repository handle per worker.

use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// An open repository and where the indexed root sits in its work tree.
pub(crate) struct Opened {
    pub repo: gix::ThreadSafeRepository,
    /// Root relative to the work tree, with a trailing `/` (`""` = the whole repo).
    pub prefix: String,
    pub head: Option<gix::ObjectId>,
    pub branch: Option<String>,
    pub detached: bool,
    pub shallow: bool,
    /// Fingerprint of the shallow boundary (the commits whose parents are missing), `None`
    /// when the clone is complete. `git fetch --deepen` changes it without moving HEAD.
    pub boundary: Option<String>,
}

/// What [`open`] found.
pub(crate) enum Found {
    /// No `.git` in the root or any of its parents.
    NoGit,
    Repo(Box<Opened>),
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Whether `root` or one of its parents holds a `.git` (dir or file).
fn has_dot_git(root: &Path) -> bool {
    root.ancestors().any(|p| p.join(".git").exists())
}

/// Open the repository that holds `root`. A folder outside any repository is
/// [`Found::NoGit`]; a repository that cannot be read is an error.
pub(crate) fn open(root: &Path) -> Result<Found, String> {
    let abs = std::fs::canonicalize(root).map_err(err)?;
    if !has_dot_git(&abs) {
        return Ok(Found::NoGit);
    }
    let repo = gix::discover(&abs).map_err(err)?;
    let Some(workdir) = repo.workdir() else {
        // A bare repository has no files to map the history to.
        return Ok(Found::NoGit);
    };
    let workdir = std::fs::canonicalize(workdir).map_err(err)?;
    let prefix = match abs.strip_prefix(&workdir) {
        Ok(rel) => {
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if parts.is_empty() {
                String::new()
            } else {
                format!("{}/", parts.join("/"))
            }
        }
        Err(_) => String::new(),
    };
    let head = repo.head().map_err(err)?;
    let detached = head.is_detached();
    let branch = head
        .referent_name()
        .map(|n| n.shorten().to_str_lossy().into_owned());
    let head_id = if head.is_unborn() {
        None
    } else {
        Some(repo.head_commit().map_err(err)?.id)
    };
    let shallow = repo.is_shallow().map_err(err)?;
    let boundary = repo.shallow_commits().map_err(err)?.map(|commits| {
        let mut ids: Vec<String> = commits.iter().map(|id| id.to_string()).collect();
        ids.sort_unstable();
        blake3::hash(
            ids.join(
                "
",
            )
            .as_bytes(),
        )
        .to_hex()[..16]
            .to_string()
    });
    Ok(Found::Repo(Box::new(Opened {
        repo: repo.into_sync(),
        prefix,
        head: head_id,
        branch,
        detached,
        shallow,
        boundary,
    })))
}

/// Whether `ancestor` is `tip` or one of its ancestors (`false` when it no longer exists).
pub(crate) fn is_ancestor(o: &Opened, ancestor: &str, tip: gix::ObjectId) -> Result<bool, String> {
    let Ok(a) = gix::ObjectId::from_hex(ancestor.as_bytes()) else {
        return Ok(false);
    };
    if a == tip {
        return Ok(true);
    }
    let repo = o.repo.to_thread_local();
    if repo.find_commit(a).is_err() {
        return Ok(false);
    }
    Ok(repo.merge_base(a, tip).map_err(err)?.map(|id| id.detach()) == Some(a))
}

/// One path a commit changed, relative to the indexed root.
#[derive(Debug, Clone)]
pub(crate) struct Change {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: cartograph_core::ChangeKind,
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// One commit as read.
#[derive(Debug, Clone)]
pub(crate) struct Scanned {
    pub id: String,
    pub time: i64,
    pub name: String,
    pub email_hash: String,
    pub merge: bool,
    /// Paths changed in the whole repository.
    pub files: u32,
    pub changes: Vec<Change>,
}

/// Hash of an email, enough to tell people apart; the email itself is never kept.
pub(crate) fn email_hash(email: &str) -> String {
    let h = blake3::hash(email.trim().to_lowercase().as_bytes());
    h.to_hex()[..16].to_string()
}

/// Commits reachable from `head` and not from `hidden`, parents before children (a valid
/// order to append after the commits of `hidden`).
pub(crate) fn scan(
    o: &Opened,
    head: gix::ObjectId,
    hidden: Option<gix::ObjectId>,
) -> Result<Vec<Scanned>, String> {
    let repo = o.repo.to_thread_local();
    let walk = repo
        .rev_walk([head])
        .with_hidden(hidden)
        .all()
        .map_err(err)?;
    let mut infos: Vec<(gix::ObjectId, Vec<gix::ObjectId>)> = Vec::new();
    for info in walk {
        let info = info.map_err(err)?;
        infos.push((info.id, info.parent_ids.iter().copied().collect()));
    }
    let order = parents_first(&infos);
    let mailmap = repo.open_mailmap();
    let prefix = o.prefix.as_str();
    let ts = &o.repo;
    let out: Vec<Result<Scanned, String>> = order
        .par_iter()
        .map_init(
            || {
                let mut r = ts.to_thread_local();
                r.object_cache_size_if_unset(16 * 1024 * 1024);
                // One cache for rename detection, one for line counts.
                let caches = r
                    .diff_resource_cache_for_tree_diff()
                    .and_then(|a| Ok((a, r.diff_resource_cache_for_tree_diff()?)))
                    .map_err(err);
                (r, caches)
            },
            |(r, caches), &i| {
                let (renames, lines) = caches.as_mut().map_err(|e| e.clone())?;
                let (id, parents) = &infos[i];
                read_commit(r, renames, lines, &mailmap, prefix, *id, parents)
            },
        )
        .collect();
    out.into_iter().collect()
}

/// Indexes of `infos` with every parent (present in `infos`) before its children; tips and
/// first parents first, so the main line keeps its order.
fn parents_first(infos: &[(gix::ObjectId, Vec<gix::ObjectId>)]) -> Vec<usize> {
    let at: HashMap<gix::ObjectId, usize> = infos
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();
    // 0 = not seen, 1 = on the stack, 2 = emitted.
    let mut state = vec![0u8; infos.len()];
    let mut out = Vec::with_capacity(infos.len());
    // Iterative post-order DFS: (node, next parent to visit).
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for start in 0..infos.len() {
        if state[start] != 0 {
            continue;
        }
        state[start] = 1;
        stack.push((start, 0));
        while let Some(&mut (n, ref mut next)) = stack.last_mut() {
            if let Some(p) = infos[n].1.get(*next) {
                *next += 1;
                if let Some(&pi) = at.get(p)
                    && state[pi] == 0
                {
                    state[pi] = 1;
                    stack.push((pi, 0));
                }
                continue;
            }
            stack.pop();
            state[n] = 2;
            out.push(n);
        }
    }
    out
}

/// Strip the indexed root's prefix; `None` outside it or for a non-UTF-8 path.
fn under<'a>(prefix: &str, location: &'a gix::bstr::BStr) -> Option<&'a str> {
    location.to_str().ok()?.strip_prefix(prefix)
}

fn read_commit(
    repo: &gix::Repository,
    renames: &mut gix::diff::blob::Platform,
    lines: &mut gix::diff::blob::Platform,
    mailmap: &gix::mailmap::Snapshot,
    prefix: &str,
    id: gix::ObjectId,
    parents: &[gix::ObjectId],
) -> Result<Scanned, String> {
    use cartograph_core::ChangeKind;
    use gix::object::tree::diff::Change as C;
    let commit = repo.find_commit(id).map_err(err)?;
    let author = commit.author().map_err(err)?;
    let time = author.time().map(|t| t.seconds).unwrap_or(0);
    let person = mailmap.resolve(author.trim());
    let mut scanned = Scanned {
        id: id.to_string(),
        time,
        name: person.name.to_str_lossy().trim().to_string(),
        email_hash: email_hash(&person.email.to_str_lossy()),
        merge: parents.len() > 1,
        files: 0,
        changes: Vec::new(),
    };
    if scanned.merge {
        return Ok(scanned);
    }
    let tree = commit.tree().map_err(err)?;
    // A missing parent (the edge of a shallow clone) diffs against nothing, as git does.
    let base = match parents.first().map(|p| repo.find_commit(*p)) {
        Some(Ok(p)) => p.tree().map_err(err)?,
        _ => repo.empty_tree(),
    };
    let mut files = 0u32;
    let mut changes = Vec::new();
    base.changes()
        .map_err(err)?
        .options(|o| {
            o.track_path()
                .track_rewrites(Some(gix::diff::Rewrites::default()));
        })
        .for_each_to_obtain_tree_with_cache(&tree, renames, |change| {
            let mode = change.entry_mode();
            if mode.is_tree() || mode.is_commit() {
                return Ok(std::ops::ControlFlow::Continue(()));
            }
            files += 1;
            let (path, old_path, kind) = match &change {
                C::Addition { location, .. } => (under(prefix, location), None, ChangeKind::Added),
                C::Deletion { location, .. } => {
                    (under(prefix, location), None, ChangeKind::Deleted)
                }
                C::Modification { location, .. } => {
                    (under(prefix, location), None, ChangeKind::Modified)
                }
                C::Rewrite {
                    source_location,
                    location,
                    ..
                } => match (under(prefix, source_location), under(prefix, location)) {
                    (Some(from), Some(to)) => {
                        (Some(to), Some(from.to_string()), ChangeKind::Renamed)
                    }
                    // Moved in from outside the indexed root: new here.
                    (None, Some(to)) => (Some(to), None, ChangeKind::Added),
                    (Some(from), None) => (Some(from), None, ChangeKind::Deleted),
                    (None, None) => (None, None, ChangeKind::Modified),
                },
            };
            let Some(path) = path else {
                return Ok(std::ops::ControlFlow::Continue(()));
            };
            let counts = change
                .diff(lines)
                .ok()
                .and_then(|mut p| p.line_counts().ok())
                .flatten();
            lines.clear_resource_cache_keep_allocation();
            changes.push(Change {
                path: path.to_string(),
                old_path,
                kind,
                added: counts.map(|c| c.insertions),
                removed: counts.map(|c| c.removals),
            });
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .map_err(err)?;
    renames.clear_resource_cache_keep_allocation();
    scanned.files = files;
    scanned.changes = changes;
    Ok(scanned)
}

/// Indentation complexity of the HEAD version of each of `paths` (relative to the root).
pub(crate) fn head_complexity(
    o: &Opened,
    head: gix::ObjectId,
    paths: &HashSet<String>,
) -> Result<HashMap<String, u64>, String> {
    let repo = o.repo.to_thread_local();
    let tree = repo.find_commit(head).map_err(err)?.tree().map_err(err)?;
    let mut rec = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut rec).map_err(err)?;
    let wanted: Vec<(String, gix::ObjectId)> = rec
        .records
        .into_iter()
        .filter(|e| e.mode.is_blob())
        .filter_map(|e| {
            let rel = under(&o.prefix, e.filepath.as_bstr())?;
            paths.contains(rel).then(|| (rel.to_string(), e.oid))
        })
        .collect();
    let ts = &o.repo;
    Ok(wanted
        .par_iter()
        .map_init(
            || ts.to_thread_local(),
            |r, (path, oid)| {
                let c = oid
                    .attach(r)
                    .object()
                    .map(|obj| indent_complexity(&obj.data))
                    .unwrap_or(0);
                (path.clone(), c)
            },
        )
        .collect())
}

/// Sum of the indentation levels of the non-blank lines (four spaces or one tab a level): a
/// language-independent proxy for nesting (Tornhill's "whitespace complexity"). 0 for binary.
pub fn indent_complexity(data: &[u8]) -> u64 {
    if data.iter().take(8000).any(|&b| b == 0) {
        return 0;
    }
    let mut total = 0u64;
    for line in data.split(|&b| b == b'\n') {
        let (mut spaces, mut tabs) = (0u64, 0u64);
        let mut blank = true;
        for &b in line {
            match b {
                b' ' => spaces += 1,
                b'\t' => tabs += 1,
                b'\r' => {}
                _ => {
                    blank = false;
                    break;
                }
            }
        }
        if !blank {
            total += spaces / 4 + tabs;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation_complexity_counts_levels_of_non_blank_lines() {
        let src = b"fn a() {\n    if x {\n        y();\n    }\n\n        \n}\n";
        assert_eq!(indent_complexity(src), 1 + 2 + 1);
        assert_eq!(indent_complexity(b"a\n\tb\n\t\tc\r\n"), 3);
        assert_eq!(indent_complexity(b"\0binary    x"), 0);
    }

    #[test]
    fn parents_come_first() {
        let id = |n: u8| gix::ObjectId::from_bytes_or_panic(&[n; 20]);
        // 4 merges 2 and 3, both children of 1.
        let infos = vec![
            (id(4), vec![id(2), id(3)]),
            (id(3), vec![id(1)]),
            (id(2), vec![id(1)]),
            (id(1), vec![]),
        ];
        let order: Vec<gix::ObjectId> = parents_first(&infos)
            .into_iter()
            .map(|i| infos[i].0)
            .collect();
        let pos = |x| order.iter().position(|&o| o == id(x)).unwrap();
        assert_eq!(order.len(), 4);
        assert!(pos(1) < pos(2) && pos(1) < pos(3) && pos(2) < pos(4) && pos(3) < pos(4));
        assert_eq!(
            email_hash(" Ada@Example.com "),
            email_hash("ada@example.com")
        );
    }
}
