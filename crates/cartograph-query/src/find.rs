//! `find(query, kind?)`: fuzzy search over dir and file paths and external packages, and the
//! id resolution every other query uses (with "did you mean" suggestions).

use crate::QueryError;
use crate::budget::{self, Answer, fit, limits};
use crate::graph::{NodeRef, ROOT, RepoGraph};
use serde::Serialize;

/// What `find` searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FindKind {
    File,
    Dir,
    Ext,
    /// Symbols: needs the symbol layer (see [`find_symbols`]).
    Sym,
}

impl std::str::FromStr for FindKind {
    type Err = QueryError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "file" | "files" => Ok(FindKind::File),
            "dir" | "dirs" | "folder" | "folders" => Ok(FindKind::Dir),
            "ext" | "external" | "lib" | "libs" => Ok(FindKind::Ext),
            "sym" | "symbol" | "symbols" => Ok(FindKind::Sym),
            other => Err(QueryError::BadArgument(format!(
                "kind must be file, dir, ext or sym, got {other:?}"
            ))),
        }
    }
}

/// `find(query, kind=sym)`, within [`limits::FIND`]: symbols whose name or `Class.method`
/// matches, best first, with `path:line`. `size` is the number of call sites reaching it.
pub fn find_symbols(repo: &crate::Repo, query: &str) -> Result<Answer<FindResult>, QueryError> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Err(QueryError::BadArgument("query is empty".into()));
    }
    let t = &repo.symbols;
    if !t.indexed {
        return Err(QueryError::BadArgument(
            "this index has no symbols; re-run `cartograph index`".into(),
        ));
    }
    let q = q.strip_prefix("sym:").unwrap_or(&q).to_string();
    let mut hits: Vec<(f64, usize, String, bool)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, x) in t.symbols.iter().enumerate() {
        let id = x.id();
        if !seen.insert(id.clone()) {
            continue;
        }
        let hay = format!("{}#{}", x.file, x.qualified()).to_lowercase();
        if let Some((s, substring)) = score(&q, &hay, &x.name.to_lowercase()) {
            // Callers break ties: the symbol people use comes first.
            let s = s + (t.callers_of(&id).len() as f64).ln_1p() * 0.5;
            hits.push((s, i, id, substring));
        }
    }
    // As for paths: subsequence-only hits are noise once some hit holds every term.
    if hits.iter().any(|h| h.3) {
        hits.retain(|h| h.3);
    }
    hits.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.2.cmp(&b.2))
    });
    Ok(fit(
        limits::FIND,
        &[25, 20, 15, 10, 6, 3],
        |limit| FindResult {
            query: query.to_string(),
            hits: hits
                .iter()
                .take(limit)
                .map(|(_, i, id, _)| {
                    let x = &t.symbols[*i];
                    Hit {
                        id: budget::clip(id, 240),
                        kind: FindKind::Sym,
                        label: budget::clip(
                            &format!(
                                "{}:{} {} {}",
                                x.file,
                                x.line,
                                x.kind.as_str(),
                                x.qualified()
                            ),
                            200,
                        ),
                        size: t.callers_of(id).len() as u64,
                    }
                })
                .collect(),
            more: hits.len().saturating_sub(limit),
        },
        render,
    ))
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub id: String,
    pub kind: FindKind,
    /// Display form (dirs end in `/`).
    pub label: String,
    /// Files for a dir, importers for an external, LOC for a file.
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindResult {
    pub query: String,
    pub hits: Vec<Hit>,
    /// Matches left out.
    pub more: usize,
}

fn is_subsequence(q: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    q.chars().all(|c| it.any(|h| h == c))
}

/// Score of one lowercase, non-empty term against a lowercase path and name, or `None`.
/// Every character must appear in order in the path; whole-name, prefix and substring
/// matches score higher, so do contiguous runs and word starts; long paths a little lower.
fn score_term(q: &str, path: &str, name: &str) -> Option<f64> {
    let mut s = 0.0;
    if name == q {
        s += 100.0;
    } else if name.starts_with(q) {
        s += 60.0;
    } else if name.contains(q) {
        s += 40.0;
    } else if path.contains(q) {
        s += 25.0;
    } else if is_subsequence(q, name) {
        s += 10.0;
    }
    let hay: Vec<char> = path.chars().collect();
    let mut hi = 0;
    let mut prev: Option<usize> = None;
    let mut run = 0.0;
    for qc in q.chars() {
        let mut found = None;
        while hi < hay.len() {
            if hay[hi] == qc {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        let i = found?;
        let boundary = i == 0 || matches!(hay[i - 1], '/' | '-' | '_' | '.' | ' ' | ':');
        if prev.is_some_and(|p| p + 1 == i) {
            run += 1.0;
            s += 2.0 + run;
        } else {
            run = 0.0;
            s += 1.0;
        }
        if boundary {
            s += 3.0;
        }
        prev = Some(i);
    }
    s -= hay.len() as f64 * 0.05;
    Some(s.max(0.0))
}

/// Dice coefficient over character pairs: 1 for equal strings, 0 for nothing in common.
fn dice(a: &str, b: &str) -> f64 {
    let pairs = |s: &str| -> Vec<(char, char)> {
        let c: Vec<char> = s.chars().collect();
        c.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let (pa, mut pb) = (pairs(a), pairs(b));
    if pa.is_empty() || pb.is_empty() {
        return if a == b { 1.0 } else { 0.0 };
    }
    let total = pa.len() + pb.len();
    let mut common = 0;
    for p in &pa {
        if let Some(i) = pb.iter().position(|q| q == p) {
            pb.swap_remove(i);
            common += 1;
        }
    }
    2.0 * common as f64 / total as f64
}

/// Score of a query (space-separated terms, all must match), and whether every term is a
/// substring of the path (not only a subsequence).
fn score(query: &str, path: &str, name: &str) -> Option<(f64, bool)> {
    let mut total = 0.0;
    let mut any = false;
    let mut substring = true;
    for term in query.split_whitespace() {
        total += score_term(term, path, name)?;
        substring &= path.contains(term);
        any = true;
    }
    any.then_some((total, substring))
}

/// Every node matching `query`, best first (ties: shorter path, then path). When some
/// node has every term as a substring of its path, nodes that only match letter by letter
/// (`resolve` → `tests/remote_solver.rs`) are noise and left out; they stay when nothing
/// matches as a substring, for typos (`rslv`).
pub fn search(g: &RepoGraph, query: &str, kind: Option<FindKind>) -> Vec<(f64, NodeRef)> {
    let q = query.trim().to_lowercase().replace('\\', "/");
    let q = q.trim_start_matches("file:").trim_start_matches("dir:");
    let mut hits: Vec<(f64, bool, NodeRef, &str)> = Vec::new();
    if kind.is_none() || kind == Some(FindKind::File) {
        for (i, f) in g.files.iter().enumerate() {
            let lp = f.path.to_lowercase();
            if let Some((s, sub)) = score(q, &lp, &f.name.to_lowercase()) {
                hits.push((s, sub, NodeRef::File(i as u32), &f.path));
            }
        }
    }
    if kind.is_none() || kind == Some(FindKind::Dir) {
        for (i, d) in g.dirs.iter().enumerate().skip(1) {
            let lp = format!("{}/", d.path.to_lowercase());
            if let Some((s, sub)) = score(q, &lp, &d.name.to_lowercase()) {
                hits.push((s, sub, NodeRef::Dir(i as u32), &d.path));
            }
        }
    }
    if kind.is_none() || kind == Some(FindKind::Ext) {
        for (i, x) in g.externals.iter().enumerate() {
            let lp = x.name.to_lowercase();
            let name = lp.rsplit(':').next().unwrap_or(&lp).to_string();
            if let Some((s, sub)) = score(q, &lp, &name) {
                hits.push((s, sub, NodeRef::Ext(i as u32), &x.name));
            }
        }
    }
    if hits.iter().any(|h| h.1) {
        hits.retain(|h| h.1);
    }
    let mut hits: Vec<(f64, NodeRef, &str)> =
        hits.into_iter().map(|(s, _, n, p)| (s, n, p)).collect();
    hits.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.2.len().cmp(&b.2.len()))
            .then(a.2.cmp(b.2))
    });
    hits.into_iter().map(|(s, n, _)| (s, n)).collect()
}

fn hit(g: &RepoGraph, n: NodeRef) -> Hit {
    let (kind, size) = match n {
        NodeRef::File(f) => (FindKind::File, g.files[f as usize].loc),
        NodeRef::Dir(d) => (FindKind::Dir, g.dirs[d as usize].file_count),
        NodeRef::Ext(x) => (FindKind::Ext, g.ext_in[x as usize].len() as u64),
    };
    Hit {
        id: g.id(n),
        kind,
        label: g.label(n),
        size,
    }
}

/// `find(query, kind?)`, within [`limits::FIND`]. Kind `sym` searches symbols (see
/// [`find_symbols`]); no kind searches dirs, files and external packages.
pub fn find(
    repo: &crate::Repo,
    query: &str,
    kind: Option<FindKind>,
) -> Result<Answer<FindResult>, QueryError> {
    if kind == Some(FindKind::Sym) {
        return find_symbols(repo, query);
    }
    if query.trim().is_empty() {
        return Err(QueryError::BadArgument("query is empty".into()));
    }
    let g = &repo.graph;
    let all = search(g, query, kind);
    Ok(fit(
        limits::FIND,
        &[25, 20, 15, 10, 6, 3],
        |limit| FindResult {
            query: query.to_string(),
            hits: all.iter().take(limit).map(|&(_, n)| hit(g, n)).collect(),
            more: all.len().saturating_sub(limit),
        },
        render,
    ))
}

fn render(r: &FindResult) -> String {
    if r.hits.is_empty() {
        return format!("no match for {:?}\n", r.query);
    }
    let mut s = String::new();
    for h in &r.hits {
        let size = match h.kind {
            FindKind::File => format!("{} LOC", h.size),
            FindKind::Dir => format!("{} files", h.size),
            FindKind::Ext => format!("used by {} files", h.size),
            FindKind::Sym => format!("←{}", h.size),
        };
        s.push_str(&format!("{}  {size}\n", budget::clip(&h.label, 140)));
    }
    if r.more > 0 {
        s.push_str(&format!("+{} more\n", r.more));
    }
    s
}

/// Resolve what a user or agent typed into a node. Accepted: `file:<path>`, `dir:<path>`,
/// `repo:`, stored external ids (`external:npm:react`), `ext:npm:react`, `npm:react`, a bare
/// path (`src/a.ts`, `src/ui`, `src/ui/`, `.\src\a.ts`), `.` or `./` for the root, or a bare
/// package name if exactly one external has it. Anything else is an error that lists the
/// closest matches.
pub fn resolve(g: &RepoGraph, input: &str) -> Result<NodeRef, QueryError> {
    let raw = input.trim();
    let s = raw.replace('\\', "/");
    let s = s.as_str();
    if matches!(
        s,
        "" | "." | "./" | "/" | "repo:" | "dir:" | "dir:." | "dir:/"
    ) {
        return Ok(NodeRef::Dir(ROOT));
    }
    let strip = |p: &str| -> String {
        let p = p.strip_prefix("./").unwrap_or(p);
        p.trim_end_matches('/').to_string()
    };
    if let Some(p) = s.strip_prefix("file:") {
        if let Some(f) = g.file_by_path(&strip(p)) {
            return Ok(NodeRef::File(f));
        }
    } else if let Some(p) = s.strip_prefix("dir:") {
        if let Some(d) = g.dir_by_path(&strip(p)) {
            return Ok(NodeRef::Dir(d));
        }
    } else if s.starts_with("external:") {
        if let Some(x) = g.ext_by_id(s) {
            return Ok(NodeRef::Ext(x));
        }
    } else if let Some(rest) = s.strip_prefix("ext:") {
        if let Some(x) = g.ext_by_id(&format!("external:{rest}")) {
            return Ok(NodeRef::Ext(x));
        }
    } else {
        let p = strip(s);
        if !s.ends_with('/')
            && let Some(f) = g.file_by_path(&p)
        {
            return Ok(NodeRef::File(f));
        }
        if let Some(d) = g.dir_by_path(&p) {
            return Ok(NodeRef::Dir(d));
        }
        if let Some(x) = g.ext_by_id(&format!("external:{s}")) {
            return Ok(NodeRef::Ext(x));
        }
        let by_name: Vec<usize> = g
            .externals
            .iter()
            .enumerate()
            .filter(|(_, x)| x.name.rsplit_once(':').is_some_and(|(_, n)| n == s))
            .map(|(i, _)| i)
            .collect();
        if by_name.len() == 1 {
            return Ok(NodeRef::Ext(by_name[0] as u32));
        }
    }
    let q = s
        .trim_start_matches("file:")
        .trim_start_matches("dir:")
        .trim_start_matches("external:")
        .trim_start_matches("ext:");
    let q = q.strip_prefix("./").unwrap_or(q).trim_end_matches('/');
    let mut suggestions: Vec<String> = search(g, q, None)
        .into_iter()
        .take(3)
        .map(|(_, n)| g.label(n))
        .collect();
    if suggestions.is_empty() {
        // Typos (`modle.ts`): the closest names by shared letter pairs, whatever the folder.
        let last = q.rsplit('/').next().unwrap_or(q).to_lowercase();
        let mut near: Vec<(f64, NodeRef, &str)> = Vec::new();
        for (i, f) in g.files.iter().enumerate() {
            let s = dice(&last, &f.name.to_lowercase());
            if s >= 0.5 {
                near.push((s, NodeRef::File(i as u32), &f.path));
            }
        }
        for (i, d) in g.dirs.iter().enumerate().skip(1) {
            let s = dice(&last, &d.name.to_lowercase());
            if s >= 0.5 {
                near.push((s, NodeRef::Dir(i as u32), &d.path));
            }
        }
        near.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.2.cmp(b.2))
        });
        suggestions = near
            .into_iter()
            .take(3)
            .map(|(_, n, _)| g.label(n))
            .collect();
    }
    Err(QueryError::UnknownNode {
        input: raw.to_string(),
        suggestions,
    })
}
