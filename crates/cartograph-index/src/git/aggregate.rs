//! Raw history → per-file aggregates. Pure (no git, no I/O), so it runs over every stored
//! row whenever new commits arrive and is tested without a repository.
//!
//! Renames are followed by giving each file an identity that moves with it: commits are
//! replayed in `seq` order (parents before children), a rename moves the identity of its
//! source to its destination, and a path deleted and later added again gets its identity
//! back (as `git log -- <path>` counts both lives). Only identities alive at HEAD are kept.
//!
//! People: a name and an email hash written in commits are one author identity; identities
//! sharing an email hash or a (case-insensitive) name are one person, named by the spelling
//! with most commits.

use cartograph_core::{AuthorShare, ChangeKind, CoChange, FileHistory, GitRaw};
use std::collections::{BTreeMap, HashMap};

/// Pairs changed together fewer times than this are not stored.
pub const MIN_PAIR: u32 = 2;

/// What [`aggregate`] computes. `complexity` is left 0 (it needs the HEAD blobs).
#[derive(Debug, Default)]
pub struct Aggregated {
    pub files: Vec<FileHistory>,
    pub pairs: Vec<CoChange>,
    pub commits: u64,
    pub merges: u64,
    pub people: u64,
    pub oldest: i64,
    pub newest: i64,
}

#[derive(Default)]
struct Ident {
    commits: u32,
    added: u64,
    removed: u64,
    recent: u32,
    first: i64,
    last: i64,
    co_commits: u32,
    authors: HashMap<usize, u32>,
    /// Last commit counted, so two rows of one commit count once.
    last_seq: Option<u32>,
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[ra.max(rb)] = ra.min(rb);
    }
}

/// People: author index → person index, and each person's display name.
fn people(raw: &GitRaw) -> (HashMap<u32, usize>, Vec<String>) {
    let n = raw.authors.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut by_email: HashMap<&str, usize> = HashMap::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (i, a) in raw.authors.iter().enumerate() {
        if !a.email_hash.is_empty() {
            match by_email.get(a.email_hash.as_str()) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_email.insert(&a.email_hash, i);
                }
            }
        }
        let name = a.name.trim().to_lowercase();
        if !name.is_empty() {
            match by_name.get(&name) {
                Some(&j) => union(&mut parent, i, j),
                None => {
                    by_name.insert(name, i);
                }
            }
        }
    }
    let ix: HashMap<u32, usize> = raw
        .authors
        .iter()
        .enumerate()
        .map(|(i, a)| (a.id, i))
        .collect();
    // Commits per author identity, for the display name.
    let mut used = vec![0u32; n];
    for c in &raw.commits {
        if let Some(&i) = ix.get(&c.author) {
            used[i] += 1;
        }
    }
    let mut roots: BTreeMap<usize, usize> = BTreeMap::new();
    let mut person_of = HashMap::new();
    let mut best: Vec<(u32, String)> = Vec::new();
    for (i, a) in raw.authors.iter().enumerate() {
        let r = find(&mut parent, i);
        let next = roots.len();
        let p = *roots.entry(r).or_insert(next);
        if p == best.len() {
            best.push((0, String::new()));
        }
        let name = a.name.trim().to_string();
        let b = &mut best[p];
        if used[i] > b.0 || (used[i] == b.0 && (b.1.is_empty() || name < b.1)) {
            *b = (used[i], name);
        }
        person_of.insert(a.id, p);
    }
    let names = best
        .into_iter()
        .map(|(_, n)| if n.is_empty() { "(unknown)".into() } else { n })
        .collect();
    (person_of, names)
}

/// Aggregate `raw` with a "recent" window of `window_days` ending at the newest commit and
/// co-change limited to commits of at most `max_changeset` files.
pub fn aggregate(raw: &GitRaw, window_days: u32, max_changeset: u32) -> Aggregated {
    let (person_of, names) = people(raw);
    let newest = raw.commits.iter().map(|c| c.time).max().unwrap_or(0);
    let oldest = raw.commits.iter().map(|c| c.time).min().unwrap_or(0);
    let since = newest - i64::from(window_days) * 86_400;
    let mut by_seq: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, ch) in raw.changes.iter().enumerate() {
        by_seq.entry(ch.seq).or_default().push(i);
    }
    let mut commits: Vec<&cartograph_core::GitCommit> = raw.commits.iter().collect();
    commits.sort_by_key(|c| c.seq);

    let mut idents: Vec<Ident> = Vec::new();
    let mut live: HashMap<String, usize> = HashMap::new();
    let mut dead: HashMap<String, usize> = HashMap::new();
    let mut pairs: HashMap<(usize, usize), (u32, i64)> = HashMap::new();
    let new_ident = |idents: &mut Vec<Ident>| {
        idents.push(Ident::default());
        idents.len() - 1
    };
    for c in &commits {
        if c.merge {
            continue;
        }
        let Some(rows) = by_seq.get(&c.seq) else {
            continue;
        };
        let person = person_of.get(&c.author).copied();
        let eligible = c.files <= max_changeset;
        let mut touched: Vec<usize> = Vec::new();
        for &ri in rows {
            let ch = &raw.changes[ri];
            let id = match ch.kind {
                ChangeKind::Added => match dead.remove(&ch.path) {
                    Some(id) => id,
                    None => new_ident(&mut idents),
                },
                ChangeKind::Modified | ChangeKind::Deleted => match live.get(&ch.path) {
                    Some(&id) => id,
                    None => dead
                        .remove(&ch.path)
                        .unwrap_or_else(|| new_ident(&mut idents)),
                },
                ChangeKind::Renamed => {
                    let from = ch.old_path.as_deref().unwrap_or_default();
                    match live.remove(from).or_else(|| dead.remove(from)) {
                        Some(id) => id,
                        None => new_ident(&mut idents),
                    }
                }
            };
            match ch.kind {
                ChangeKind::Deleted => {
                    live.remove(&ch.path);
                    dead.insert(ch.path.clone(), id);
                }
                _ => {
                    if let Some(prev) = live.insert(ch.path.clone(), id)
                        && prev != id
                    {
                        // Overwritten by a rename onto an existing path: the old one ends.
                        dead.remove(&ch.path);
                    }
                }
            }
            let x = &mut idents[id];
            x.added += u64::from(ch.added.unwrap_or(0));
            x.removed += u64::from(ch.removed.unwrap_or(0));
            if x.last_seq == Some(c.seq) {
                continue;
            }
            x.last_seq = Some(c.seq);
            x.commits += 1;
            if x.commits == 1 {
                x.first = c.time;
            }
            x.first = x.first.min(c.time);
            x.last = x.last.max(c.time);
            if c.time >= since {
                x.recent += 1;
            }
            if eligible {
                x.co_commits += 1;
                touched.push(id);
            }
            if let Some(p) = person {
                *x.authors.entry(p).or_default() += 1;
            }
        }
        if eligible && touched.len() >= 2 {
            touched.sort_unstable();
            touched.dedup();
            for i in 0..touched.len() {
                for j in i + 1..touched.len() {
                    let e = pairs.entry((touched[i], touched[j])).or_insert((0, 0));
                    e.0 += 1;
                    e.1 = e.1.max(c.time);
                }
            }
        }
    }

    let path_of: HashMap<usize, &String> = live.iter().map(|(p, &id)| (id, p)).collect();
    let mut files: Vec<FileHistory> = live
        .iter()
        .filter(|(_, id)| idents[**id].commits > 0)
        .map(|(path, &id)| {
            let x = &idents[id];
            let mut authors: Vec<AuthorShare> = x
                .authors
                .iter()
                .map(|(&p, &n)| AuthorShare {
                    name: names[p].clone(),
                    commits: n,
                })
                .collect();
            authors.sort_by(|a, b| b.commits.cmp(&a.commits).then(a.name.cmp(&b.name)));
            FileHistory {
                path: path.clone(),
                commits: x.commits,
                added: x.added,
                removed: x.removed,
                recent: x.recent,
                first_time: x.first,
                last_time: x.last,
                co_commits: x.co_commits,
                complexity: 0,
                authors,
            }
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out_pairs: Vec<CoChange> = pairs
        .into_iter()
        .filter(|(_, (n, _))| *n >= MIN_PAIR)
        .filter_map(|((x, y), (together, last_time))| {
            let (a, b) = (path_of.get(&x)?, path_of.get(&y)?);
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            Some(CoChange {
                a: (*a).clone(),
                b: (*b).clone(),
                together,
                last_time,
            })
        })
        .collect();
    out_pairs.sort_by(|x, y| (&x.a, &x.b).cmp(&(&y.a, &y.b)));
    Aggregated {
        files,
        pairs: out_pairs,
        commits: raw.commits.len() as u64,
        merges: raw.commits.iter().filter(|c| c.merge).count() as u64,
        people: names.len() as u64,
        oldest,
        newest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartograph_core::{GitAuthor, GitChange, GitCommit};

    fn commit(seq: u32, time: i64, author: u32, files: u32) -> GitCommit {
        GitCommit {
            seq,
            id: format!("{seq:040}"),
            time,
            author,
            files,
            merge: false,
        }
    }

    fn change(seq: u32, path: &str, kind: ChangeKind, old: Option<&str>) -> GitChange {
        GitChange {
            seq,
            path: path.into(),
            old_path: old.map(str::to_string),
            kind,
            added: Some(2),
            removed: Some(1),
        }
    }

    fn author(id: u32, name: &str, email: &str) -> GitAuthor {
        GitAuthor {
            id,
            name: name.into(),
            email_hash: email.into(),
        }
    }

    #[test]
    fn follows_renames_joins_people_and_skips_huge_commits_for_pairs() {
        use ChangeKind::*;
        let day = 86_400;
        let raw = GitRaw {
            commits: vec![
                commit(0, 0, 0, 2),
                commit(1, 10 * day, 1, 2),
                commit(2, 200 * day, 2, 2),
                // Huge: counts for churn, not for co-change.
                commit(3, 201 * day, 0, 60),
                GitCommit {
                    merge: true,
                    ..commit(4, 202 * day, 0, 0)
                },
            ],
            authors: vec![
                author(0, "Ada", "h-ada"),
                // Same email, another spelling: one person.
                author(1, "ada l.", "h-ada"),
                // Same name (case aside), another email: one person too.
                author(2, "ADA", "h-other"),
            ],
            changes: vec![
                change(0, "a.ts", Added, None),
                change(0, "b.ts", Added, None),
                change(1, "a.ts", Modified, None),
                change(1, "b.ts", Modified, None),
                change(2, "c.ts", Renamed, Some("a.ts")),
                change(2, "b.ts", Modified, None),
                change(3, "c.ts", Modified, None),
                change(3, "b.ts", Deleted, None),
            ],
        };
        let a = aggregate(&raw, 90, 50);
        assert_eq!(
            a.files.len(),
            1,
            "b.ts is deleted, a.ts renamed: {:?}",
            a.files
        );
        let c = &a.files[0];
        assert_eq!(c.path, "c.ts");
        assert_eq!(c.commits, 4, "a.ts history follows into c.ts");
        assert_eq!(c.co_commits, 3);
        assert_eq!(
            c.recent, 2,
            "commits 2 and 3 are within 90 days of the newest"
        );
        assert_eq!((c.added, c.removed), (8, 4));
        assert_eq!(c.authors.len(), 1, "{:?}", c.authors);
        assert_eq!(c.authors[0].commits, 4);
        assert_eq!(a.people, 1);
        assert_eq!((a.commits, a.merges), (5, 1));
        // b.ts is gone, so no pair survives.
        assert!(a.pairs.is_empty());
    }

    #[test]
    fn a_deleted_path_added_again_keeps_its_history_and_pairs_count_shared_commits() {
        use ChangeKind::*;
        let raw = GitRaw {
            commits: (0..5).map(|s| commit(s, i64::from(s), 0, 2)).collect(),
            authors: vec![author(0, "Ada", "h")],
            changes: vec![
                change(0, "x.ts", Added, None),
                change(0, "y.ts", Added, None),
                change(1, "x.ts", Deleted, None),
                change(2, "x.ts", Added, None),
                change(2, "y.ts", Modified, None),
                change(3, "x.ts", Modified, None),
                change(3, "y.ts", Modified, None),
                change(4, "y.ts", Modified, None),
            ],
        };
        let a = aggregate(&raw, 90, 50);
        let x = a.files.iter().find(|f| f.path == "x.ts").unwrap();
        assert_eq!(x.commits, 4);
        assert_eq!(
            a.pairs,
            vec![CoChange {
                a: "x.ts".into(),
                b: "y.ts".into(),
                together: 3,
                last_time: 3,
            }]
        );
    }
}
