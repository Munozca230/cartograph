//! `explain(node)`: one file, folder or library within [`limits::EXPLAIN`]: role (kind,
//! language, LOC, layer, entry point), what it imports grouped by folder, what imports it,
//! libraries, findings touching it, its main symbols (a file) or symbol counts (a folder),
//! and its git history (a file: churn, last change, owners and the files it usually changes
//! with; `git_note` says why there is none). `annotation` is present and `null`.
//! `symbols` is `null` only for a library or an index without a symbol layer.

use crate::budget::{Answer, clip, fit, limits};
use crate::graph::{DirIx, FileIx, NodeRef, RepoGraph};
use crate::kind::FileKind;
use crate::list::touches;
use crate::text::num;
use crate::text::short;
use crate::{QueryError, Repo};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Files of one folder in a list of imports or importers.
#[derive(Debug, Clone, Serialize)]
pub struct FolderGroup {
    pub dir: String,
    pub count: usize,
    /// File names (not paths) shown; the rest is `count - files.len()`.
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Grouped {
    /// Distinct files (for a folder: file pairs crossing its border).
    pub total: usize,
    pub folders: Vec<FolderGroup>,
    pub folders_more: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingRef {
    pub id: String,
    pub severity: &'static str,
    pub title: String,
}

/// One author of a file (a name, never an email).
#[derive(Debug, Clone, Serialize)]
pub struct Owner {
    pub name: String,
    pub commits: u32,
    /// Share of the file's commits, in %.
    pub share: u32,
}

/// A file that usually changes with this one.
#[derive(Debug, Clone, Serialize)]
pub struct CoPartner {
    pub id: String,
    /// Commits that changed both.
    pub together: u32,
    /// Share of this file's commits that also changed it, in %.
    pub confidence: u32,
}

/// Git history of a file (renames followed; merges and, for co-change, huge commits left
/// out).
#[derive(Debug, Clone, Serialize)]
pub struct Git {
    /// Non-merge commits that touched it.
    pub commits: u32,
    pub added: u64,
    pub removed: u64,
    /// Commits in the last `window_days` up to the newest commit of the repo.
    pub recent: u32,
    pub window_days: u32,
    pub first_change: String,
    pub last_change: String,
    pub authors: usize,
    pub owners: Vec<Owner>,
    /// Fewest authors making 50 % and 80 % of the commits.
    pub bus_factor_50: usize,
    pub bus_factor_80: usize,
    pub co_change: Vec<CoPartner>,
    pub co_change_more: usize,
    /// The history is partial (a shallow clone).
    pub partial: bool,
}

/// The note `annotate` left: the description, stale when the content changed.
#[derive(Debug, Clone, Serialize)]
pub struct Annotation {
    pub text: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Explain {
    pub id: String,
    /// `file`, `dir` or `external`.
    pub node: &'static str,
    pub lang: Option<String>,
    pub loc: u64,
    /// Files (for a folder).
    pub files: u64,
    pub kind: Option<FileKind>,
    pub layer: Option<String>,
    pub entry: Option<&'static str>,
    pub unresolved: u64,
    pub imports: Grouped,
    /// Files other than tests that import it.
    pub imported_by: Grouped,
    /// Test files that import it.
    pub tested_by: Grouped,
    pub libraries: Vec<String>,
    pub libraries_more: usize,
    /// Biggest subfolders (for a folder).
    pub subfolders: Vec<String>,
    pub subfolders_more: usize,
    pub findings: Vec<FindingRef>,
    pub findings_more: usize,
    /// Symbols defined here (`None` for a library or an index without symbols).
    pub symbols: Option<crate::symbols::ExplainSymbols>,
    /// Git history (a file with history only).
    pub git: Option<Git>,
    /// Why `git` is empty, or, with `git`, that it is the history kept from before a
    /// failed git read.
    pub git_note: Option<String>,
    /// The note `annotate` left on the node (filled by [`explain_with`]).
    pub annotation: Option<Annotation>,
    /// Bridges (IPC channels, exposed preload functions, a member's package, HTTP) that
    /// leave or reach it: links no import states.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<crate::bridges::Link>,
    #[serde(skip_serializing_if = "crate::explain::is_zero")]
    pub bridges_more: usize,
    /// Its bridge endpoints left without the other side that need attention: an HTTP call
    /// no route serves or with the wrong method, an IPC channel no handler serves, a call
    /// the preload does not expose.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bridges_left: Vec<crate::bridges::EndpointRow>,
    #[serde(skip_serializing_if = "crate::explain::is_zero")]
    pub bridges_left_more: usize,
}

pub(crate) fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn group(g: &RepoGraph, files: &[FileIx], max_folders: usize, max_names: usize) -> Grouped {
    let mut by: BTreeMap<DirIx, Vec<FileIx>> = BTreeMap::new();
    for &f in files {
        by.entry(g.files[f as usize].dir).or_default().push(f);
    }
    let mut rows: Vec<(DirIx, Vec<FileIx>)> = by.into_iter().collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    let folders_more = rows.len().saturating_sub(max_folders);
    Grouped {
        total: files.len(),
        folders: rows
            .into_iter()
            .take(max_folders)
            .map(|(d, fs)| FolderGroup {
                dir: g.id(NodeRef::Dir(d)),
                count: fs.len(),
                files: fs
                    .iter()
                    .take(max_names)
                    .map(|&f| g.files[f as usize].name.clone())
                    .collect(),
            })
            .collect(),
        folders_more,
    }
}

pub fn explain(repo: &Repo, node: &str) -> Result<Answer<Explain>, QueryError> {
    explain_with(repo, node, |_| None)
}

/// [`explain`] with the note of the resolved node: `annotation` gets its stable id
/// (`file:…`, `dir:…`) and returns the note, if any. Notes must be short (the MCP
/// `annotate` tool caps them); the budget makes room for one by listing fewer items.
pub fn explain_with(
    repo: &Repo,
    node: &str,
    annotation: impl FnOnce(&str) -> Option<Annotation>,
) -> Result<Answer<Explain>, QueryError> {
    let g = &repo.graph;
    let n = repo.resolve(node)?;
    let annotation = annotation(&g.id(n));
    let links = match n {
        NodeRef::Ext(_) => Vec::new(),
        _ => {
            let paths: std::collections::BTreeSet<String> = g
                .files_of(n)
                .into_iter()
                .map(|f| g.path(f).to_string())
                .collect();
            crate::bridges::links(&repo.symbols.bridges, &paths, true, true)
        }
    };
    let left = match n {
        NodeRef::Ext(_) => Vec::new(),
        _ => {
            let paths: std::collections::BTreeSet<String> = g
                .files_of(n)
                .into_iter()
                .map(|f| g.path(f).to_string())
                .collect();
            crate::bridges::left(&repo.symbols.bridges, &paths)
        }
    };
    let findings: Vec<FindingRef> = repo
        .findings()
        .iter()
        .filter(|f| touches(g, f, n))
        .map(|f| FindingRef {
            id: f.id.clone(),
            severity: if f.violation {
                "violation"
            } else {
                f.severity.as_str()
            },
            title: crate::budget::clip(&f.title, 200),
        })
        .collect();

    // The node's files and its imports/importers across its border.
    let own: Vec<FileIx> = g.files_of(n);
    let inside: BTreeSet<FileIx> = own.iter().copied().collect();
    let mut out_files: BTreeSet<FileIx> = BTreeSet::new();
    let mut in_files: BTreeSet<FileIx> = BTreeSet::new();
    let mut libs: BTreeMap<u32, usize> = BTreeMap::new();
    for &f in &own {
        for d in &g.out[f as usize] {
            if !inside.contains(&d.file) {
                out_files.insert(d.file);
            }
        }
        for d in &g.inn[f as usize] {
            if !inside.contains(&d.file) {
                in_files.insert(d.file);
            }
        }
        for &x in &g.ext_out[f as usize] {
            *libs.entry(x).or_default() += 1;
        }
    }
    let out_files: Vec<FileIx> = out_files.into_iter().collect();
    let mut in_files: Vec<FileIx> = in_files.into_iter().collect();
    if let NodeRef::Ext(x) = n {
        in_files = g.ext_in[x as usize].clone();
    }
    let (test_files, in_files): (Vec<FileIx>, Vec<FileIx>) = in_files
        .into_iter()
        .partition(|&f| g.files[f as usize].kind == FileKind::Test);
    let mut libs: Vec<(usize, u32)> = libs.into_iter().map(|(x, c)| (c, x)).collect();
    libs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    let (lang, loc, files, kind, entry, unresolved, layer_path) = match n {
        NodeRef::File(f) => {
            let file = &g.files[f as usize];
            (
                file.lang.clone(),
                file.loc,
                1,
                Some(file.kind),
                file.entry,
                file.unresolved as u64,
                Some(file.path.clone()),
            )
        }
        NodeRef::Dir(d) => {
            let dir = &g.dirs[d as usize];
            let unresolved = own
                .iter()
                .map(|&f| g.files[f as usize].unresolved as u64)
                .sum();
            (
                None,
                dir.loc,
                dir.file_count,
                None,
                None,
                unresolved,
                Some(format!("{}/x", dir.path)),
            )
        }
        NodeRef::Ext(_) => (None, 0, 0, None, None, 0, None),
    };
    let layer = layer_path.and_then(|p| repo.config.layer_name(&p));
    let mut subs: Vec<DirIx> = match n {
        NodeRef::Dir(d) => g.dirs[d as usize].dirs.clone(),
        _ => Vec::new(),
    };
    subs.sort_by(|a, b| {
        g.dirs[*b as usize]
            .loc
            .cmp(&g.dirs[*a as usize].loc)
            .then(a.cmp(b))
    });

    Ok(fit(
        limits::EXPLAIN,
        &[10, 8, 6, 5, 4, 3, 2, 1],
        |limit| Explain {
            id: g.id(n),
            node: match n {
                NodeRef::File(_) => "file",
                NodeRef::Dir(_) => "dir",
                NodeRef::Ext(_) => "external",
            },
            lang: lang.clone(),
            loc,
            files,
            kind,
            layer: layer.clone(),
            entry,
            unresolved,
            imports: group(g, &out_files, limit, limit.min(4)),
            imported_by: group(g, &in_files, limit, limit.min(4)),
            tested_by: group(g, &test_files, limit.min(3), limit.min(2)),
            libraries: libs
                .iter()
                .take(limit)
                .map(|&(_, x)| g.externals[x as usize].id.clone())
                .collect(),
            libraries_more: libs.len().saturating_sub(limit),
            subfolders: subs
                .iter()
                .take(limit)
                .map(|&d| g.id(NodeRef::Dir(d)))
                .collect(),
            subfolders_more: subs.len().saturating_sub(limit),
            findings: findings.iter().take(limit).cloned().collect(),
            findings_more: findings.len().saturating_sub(limit),
            symbols: crate::symbols::for_explain(repo, n, limit),
            git: git_of(repo, n, limit),
            git_note: git_note(repo, n),
            annotation: annotation.clone(),
            bridges: links.iter().take(limit.min(6)).cloned().collect(),
            bridges_more: links.len().saturating_sub(limit.min(6)),
            bridges_left: left.iter().take(limit.min(4)).cloned().collect(),
            bridges_left_more: left.len().saturating_sub(limit.min(4)),
        },
        render,
    ))
}

/// `part` of `whole` in % (rounded; 0 of nothing).
fn pct(part: u32, whole: u32) -> u32 {
    (part * 100 + whole / 2).checked_div(whole).unwrap_or(0)
}

fn git_of(repo: &Repo, n: NodeRef, limit: usize) -> Option<Git> {
    let NodeRef::File(f) = n else {
        return None;
    };
    let h = repo.git.of(f)?;
    let g = &repo.graph;
    let total: u32 = h.authors.iter().map(|a| a.commits).sum();
    let owners_max = limit.min(3);
    let partners = &repo.git.partners[f as usize];
    let co_max = limit.min(5);
    Some(Git {
        commits: h.commits,
        added: h.added,
        removed: h.removed,
        recent: h.recent,
        window_days: repo.git.status.as_ref().map_or(0, |s| s.window_days),
        first_change: crate::history::date(h.first_time),
        last_change: crate::history::date(h.last_time),
        authors: h.authors.len(),
        owners: h
            .authors
            .iter()
            .take(owners_max)
            .map(|a| Owner {
                name: clip(&a.name, 60),
                commits: a.commits,
                share: pct(a.commits, total),
            })
            .collect(),
        bus_factor_50: crate::history::bus_factor(&h.authors, 0.5),
        bus_factor_80: crate::history::bus_factor(&h.authors, 0.8),
        co_change: partners
            .iter()
            .take(co_max)
            .map(|p| CoPartner {
                id: g.id(NodeRef::File(p.file)),
                together: p.together,
                confidence: (repo.git.confidence(f, p) * 100.0).round() as u32,
            })
            .collect(),
        co_change_more: partners.len().saturating_sub(co_max),
        partial: repo.git.shallow(),
    })
}

fn git_note(repo: &Repo, n: NodeRef) -> Option<String> {
    if let NodeRef::Ext(_) = n {
        return None;
    }
    if let Some(why) = repo.git.unavailable() {
        return Some(why);
    }
    // After a failed read, only a file with kept history shows "the history kept from
    // before"; the others say what they show.
    let failed = repo.git.read_failure();
    match n {
        NodeRef::File(f) if repo.git.of(f).is_none() => Some(match failed {
            None => "no commit touches it (new or untracked)".into(),
            Some(e) => format!(
                "no commit touches it in the history kept from before the last git read \
                 failed ({e})"
            ),
        }),
        NodeRef::File(_) => repo.git.read_error(),
        NodeRef::Dir(_) => {
            let hint = format!(
                "per file; hottest here: `cartograph query hotspots --within {}`",
                repo.graph.id(n)
            );
            Some(match failed {
                None => hint,
                Some(e) => format!("{hint} (the last git read failed: {e})"),
            })
        }
        NodeRef::Ext(_) => None,
    }
}

fn render_git(s: &mut String, e: &Explain) {
    let Some(git) = &e.git else {
        s.push_str(&format!(
            "git: {}\n",
            e.git_note.as_deref().unwrap_or("no history")
        ));
        return;
    };
    s.push_str(&format!(
        "git: {} commits (+{} −{} lines), {} in the last {} days · first {} · last {}{}\n",
        git.commits,
        num(git.added),
        num(git.removed),
        git.recent,
        git.window_days,
        git.first_change,
        git.last_change,
        if git.partial {
            " · shallow clone: partial history"
        } else {
            ""
        }
    ));
    if let Some(note) = &e.git_note {
        s.push_str(&format!(
            "git history {note}
"
        ));
    }
    let owners: Vec<String> = git
        .owners
        .iter()
        .map(|o| format!("{} {}%", o.name, o.share))
        .collect();
    s.push_str(&format!(
        "owners: {}{} · bus factor {} (50%), {} (80%)\n",
        owners.join(", "),
        crate::budget::more(git.authors.saturating_sub(git.owners.len())),
        git.bus_factor_50,
        git.bus_factor_80
    ));
    if git.co_change.is_empty() {
        s.push_str("changes with: no file twice or more\n");
    } else {
        let list: Vec<String> = git
            .co_change
            .iter()
            .map(|p| {
                format!(
                    "{} {}× ({}%)",
                    clip(&short(&p.id), 100),
                    p.together,
                    p.confidence
                )
            })
            .collect();
        s.push_str(&format!(
            "changes with: {}{}\n",
            list.join(", "),
            crate::budget::more(git.co_change_more)
        ));
    }
}

fn render_group(s: &mut String, title: &str, gr: &Grouped) {
    if gr.total == 0 {
        s.push_str(&format!("{title}: none\n"));
        return;
    }
    s.push_str(&format!("{title} ({}):\n", crate::text::files(gr.total)));
    for f in &gr.folders {
        let names = if f.files.is_empty() {
            String::new()
        } else {
            let extra = f.count.saturating_sub(f.files.len());
            format!(
                ": {}{}",
                f.files.join(", "),
                if extra > 0 {
                    format!(" +{extra}")
                } else {
                    String::new()
                }
            )
        };
        s.push_str(&format!(
            "  {} {}{}\n",
            clip(&short(&f.dir), 100),
            f.count,
            clip(&names, 160)
        ));
    }
    if gr.folders_more > 0 {
        s.push_str(&format!("  +{} more folders\n", gr.folders_more));
    }
}

fn render(e: &Explain) -> String {
    let mut s = String::new();
    let mut head = format!("{} {}", e.node, short(&e.id));
    if let Some(l) = &e.lang {
        head.push_str(&format!(" · {l}"));
    }
    if e.node == "dir" {
        head.push_str(&format!(" · {} files", e.files));
    }
    if e.node != "external" {
        head.push_str(&format!(" · {} LOC", num(e.loc)));
    }
    if let Some(k) = e.kind {
        head.push_str(&format!(" · {}", k.as_str()));
    }
    if let Some(l) = &e.layer {
        head.push_str(&format!(" · layer {l}"));
    }
    if let Some(en) = e.entry {
        head.push_str(&format!(" · entry ({en})"));
    }
    s.push_str(&head);
    s.push('\n');
    if !e.subfolders.is_empty() {
        let list: Vec<String> = e.subfolders.iter().map(|d| short(d)).collect();
        s.push_str(&format!(
            "subfolders: {}{}\n",
            list.join(", "),
            crate::budget::more(e.subfolders_more)
        ));
    }
    if e.node != "external" {
        render_group(&mut s, "imports", &e.imports);
    }
    render_group(&mut s, "imported by", &e.imported_by);
    if e.tested_by.total > 0 {
        render_group(&mut s, "tested by", &e.tested_by);
    }
    if !e.libraries.is_empty() {
        let list: Vec<String> = e.libraries.iter().map(|x| short(x)).collect();
        s.push_str(&format!(
            "libraries: {}{}\n",
            list.join(", "),
            crate::budget::more(e.libraries_more)
        ));
    }
    if e.unresolved > 0 {
        s.push_str(&format!("unresolved imports: {}\n", e.unresolved));
    }
    if e.findings.is_empty() {
        s.push_str("findings: none\n");
    } else {
        s.push_str("findings:\n");
        for f in &e.findings {
            s.push_str(&format!(
                "  [{}] {}: {}\n",
                f.severity,
                clip(&f.id, 120),
                clip(&f.title, 160)
            ));
        }
        if e.findings_more > 0 {
            s.push_str(&format!("  +{} more\n", e.findings_more));
        }
    }
    crate::bridges::render_left(&mut s, &e.bridges_left, e.bridges_left_more);
    crate::bridges::render_links(&mut s, &e.bridges, e.bridges_more);
    if e.node != "external" {
        s.push_str(&crate::symbols::explain_line(&e.id, e.symbols.as_ref()));
        s.push('\n');
        render_git(&mut s, e);
        s.push_str(&annotation_line(e.annotation.as_ref()));
    }
    s
}

/// The note line: the note, marked when the code changed after it was written.
fn annotation_line(a: Option<&Annotation>) -> String {
    match a {
        None => "note: none\n".to_string(),
        Some(a) if a.stale => format!(
            "note (stale: the code changed after it was written): {}\n",
            clip(&a.text, 600)
        ),
        Some(a) => format!("note: {}\n", clip(&a.text, 600)),
    }
}
