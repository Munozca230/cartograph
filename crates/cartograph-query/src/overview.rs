//! `overview(path?, depth)`: the map of a folder (the repo by default) within
//! [`limits::OVERVIEW`]: folder tree with files, LOC, dominant kind and findings; entry
//! points; the most depended-on files; the main libraries; findings by kind.

use crate::budget::{Answer, clip, fit, limits};
use crate::findings::{Summary, summarize};
use crate::graph::{DirIx, FileIx, NodeRef, ROOT, RepoGraph};
use crate::kind::FileKind;
use crate::list::{summary_line, touches};
pub(crate) use crate::test_trees::in_test_tree;
use crate::text::num;
pub(crate) use crate::text::short;
use crate::{QueryError, Repo};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct Lang {
    pub lang: String,
    pub files: u64,
    pub loc: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: String,
    /// Levels below the overview's root (1 = direct child).
    pub level: u32,
    pub files: u64,
    pub loc: u64,
    /// Most common kind of its files: code, test, script, style or data.
    pub kind: FileKind,
    /// Findings touching something inside.
    pub findings: usize,
    /// Subfolders not listed.
    #[serde(skip_serializing_if = "is_zero")]
    pub more: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    pub why: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ranked {
    pub id: String,
    pub n: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub root: String,
    pub repo: String,
    pub files: u64,
    pub loc: u64,
    pub languages: Vec<Lang>,
    pub tree: Vec<Row>,
    /// Top-level folders not listed.
    pub tree_more: usize,
    /// Files directly in the root folder.
    pub loose_files: usize,
    pub entries: Vec<Entry>,
    pub entries_more: usize,
    /// Most depended-on files (`n` = files depending on it).
    pub hubs: Vec<Ranked>,
    /// Most used libraries (`n` = files importing it).
    pub libraries: Vec<Ranked>,
    pub libraries_more: usize,
    pub findings: Summary,
    pub unresolved_imports: u64,
    /// Where flows start (main, routes, commands, Godot scenes), from the symbol layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flows: Option<crate::symbols::Flows>,
    /// What an imported SCIP index made precise (N3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scip: Option<crate::symbols::ScipOverview>,
    /// In a workspace, seen from its root: the dependencies between members (`n` = file
    /// pairs), most first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub across_members: Vec<MemberLink>,
}

/// Files of member `from` depending on files of member `to`.
#[derive(Debug, Clone, Serialize)]
pub struct MemberLink {
    pub from: String,
    pub to: String,
    pub n: usize,
}

/// The dependencies between the members of a workspace (none for a single repo).
fn member_links(g: &RepoGraph) -> Vec<MemberLink> {
    let mut pairs: std::collections::BTreeMap<(&str, &str), usize> = Default::default();
    for (a, deps) in g.out.iter().enumerate() {
        let Some(ma) = g.members.member_of(&g.files[a].path) else {
            continue;
        };
        for d in deps {
            if let Some(mb) = g.members.member_of(&g.files[d.file as usize].path)
                && mb != ma
            {
                *pairs.entry((ma, mb)).or_default() += 1;
            }
        }
    }
    let mut out: Vec<MemberLink> = pairs
        .into_iter()
        .map(|((from, to), n)| MemberLink {
            from: from.to_string(),
            to: to.to_string(),
            n,
        })
        .collect();
    out.sort_by(|a, b| {
        b.n.cmp(&a.n)
            .then(a.from.cmp(&b.from))
            .then(a.to.cmp(&b.to))
    });
    out
}

/// Entry reasons worth listing (tests, scripts and config are entries too, but noise here).
fn listed_entry(why: &str) -> bool {
    !matches!(
        why,
        "test"
            | "script"
            | "config/data"
            | "type declarations"
            | "story"
            | "tool entry"
            | "asset or example"
    )
}

fn entry_rank(why: &str) -> u8 {
    match why {
        "godot main scene" => 0,
        "crate root" | "package entry" => 1,
        "godot autoload" => 2,
        "entry name" => 3,
        "route" => 4,
        _ => 5,
    }
}

/// A folder holding more than this share of its parent's files opens one level more: an
/// overview of `./` where `src/` holds everything should show what is inside `src/`.
const DOMINANT_SHARE: f64 = 0.6;

pub fn overview(
    repo: &Repo,
    path: Option<&str>,
    depth: u32,
) -> Result<Answer<Overview>, QueryError> {
    let g = &repo.graph;
    let root = match path {
        None => ROOT,
        Some(p) => match repo.resolve(p)? {
            NodeRef::Dir(d) => d,
            other => {
                return Err(QueryError::BadArgument(format!(
                    "overview takes a folder; {} is not one (use explain)",
                    g.label(other)
                )));
            }
        },
    };
    let depth = depth.clamp(1, 4);
    let files = g.files_under(root);
    let base_depth = g.dirs[root as usize].depth;

    let mut langs: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for &f in &files {
        let file = &g.files[f as usize];
        if let Some(l) = &file.lang {
            let e = langs.entry(l.clone()).or_default();
            e.0 += 1;
            e.1 += file.loc;
        }
    }
    let mut languages: Vec<Lang> = langs
        .into_iter()
        .map(|(lang, (files, loc))| Lang { lang, files, loc })
        .collect();
    languages.sort_by(|a, b| b.loc.cmp(&a.loc).then(a.lang.cmp(&b.lang)));
    languages.truncate(6);

    let findings: Vec<_> = repo
        .findings()
        .iter()
        .filter(|f| root == ROOT || touches(g, f, NodeRef::Dir(root)))
        .collect();
    let summary = summarize(findings.iter().copied());

    // Folder rows, depth-first, children by LOC.
    let rows_for = |per_dir: usize| -> (Vec<Row>, usize) {
        let mut rows = Vec::new();
        let mut stack: Vec<DirIx> = Vec::new();
        let kids = |d: DirIx| -> Vec<DirIx> {
            let mut k = g.dirs[d as usize].dirs.clone();
            k.sort_by(|a, b| {
                g.dirs[*b as usize]
                    .loc
                    .cmp(&g.dirs[*a as usize].loc)
                    .then(a.cmp(b))
            });
            k
        };
        let mut first: Vec<DirIx> = kids(root)
            .into_iter()
            .filter(|&d| g.dirs[d as usize].file_count > 0)
            .collect();
        let top_more = first.len().saturating_sub(per_dir.max(1) * 2);
        first.truncate(per_dir.max(1) * 2);
        stack.extend(first.into_iter().rev());
        while let Some(d) = stack.pop() {
            let dir = &g.dirs[d as usize];
            let level = dir.depth - base_depth;
            let mut children: Vec<DirIx> = kids(d)
                .into_iter()
                .filter(|&c| g.dirs[c as usize].file_count > 0)
                .collect();
            let parent_files = dir
                .parent
                .map_or(dir.file_count, |p| g.dirs[p as usize].file_count)
                .max(1);
            let dominant = level == depth
                && depth < 4
                && dir.file_count as f64 > DOMINANT_SHARE * parent_files as f64
                && dir
                    .parent
                    .is_some_and(|p| p == root || g.dirs[p as usize].depth - base_depth < depth);
            let (shown, more) = if level < depth || dominant {
                let more = children.len().saturating_sub(per_dir);
                children.truncate(per_dir);
                (children, more)
            } else {
                (Vec::new(), children.len())
            };
            rows.push(Row {
                id: g.id(NodeRef::Dir(d)),
                level,
                files: dir.file_count,
                loc: dir.loc,
                kind: g.dir_kind[d as usize],
                findings: findings
                    .iter()
                    .filter(|f| touches(g, f, NodeRef::Dir(d)))
                    .count(),
                more,
            });
            stack.extend(shown.into_iter().rev());
        }
        (rows, top_more)
    };

    // Files a program starts in (a `main` the symbol layer found: a Java or C# `static
    // main`, a Rust or Go `main`, a `package.json` `main`) come first, whatever their name;
    // the one that uses the most files first, so the application's `main` leads a stray
    // sample `Main.java`.
    let mains: std::collections::HashSet<&str> = repo
        .symbols
        .entries
        .iter()
        .filter(|e| e.kind == cartograph_core::EntryKind::Main)
        .map(|e| e.file.as_str())
        .collect();
    let mut entries: Vec<(u8, Option<std::cmp::Reverse<usize>>, FileIx, &'static str)> = files
        .iter()
        .filter_map(|&f| {
            let program = mains.contains(g.files[f as usize].path.as_str())
                && g.files[f as usize].kind == FileKind::Code
                && !in_test_tree(g, f, root);
            let why = match root_entry(g, f, root) {
                Some(why) => why,
                None if program => "main",
                None => return None,
            };
            let rank = if program { 0 } else { entry_rank(why) };
            let uses = program.then(|| std::cmp::Reverse(g.out[f as usize].len()));
            Some((rank, uses, f, why))
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.0, a.1, g.files[a.2 as usize].dir != ROOT, a.2).cmp(&(
            b.0,
            b.1,
            g.files[b.2 as usize].dir != ROOT,
            b.2,
        ))
    });
    let entries: Vec<(u8, FileIx, &'static str)> = entries
        .into_iter()
        .map(|(r, _, f, why)| (r, f, why))
        .collect();

    let mut hubs: Vec<(usize, FileIx)> = files
        .iter()
        .filter(|&&f| g.files[f as usize].kind == FileKind::Code)
        .map(|&f| (non_test_importers(g, f), f))
        .filter(|x| x.0 > 1)
        .collect();
    hubs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hubs.truncate(6);

    let mut libs: BTreeMap<u32, usize> = BTreeMap::new();
    for &f in &files {
        if g.files[f as usize].kind == FileKind::Test {
            continue;
        }
        for &x in &g.ext_out[f as usize] {
            *libs.entry(x).or_default() += 1;
        }
    }
    let mut libs: Vec<(usize, u32)> = libs.into_iter().map(|(x, n)| (n, x)).collect();
    libs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    let loose = g.dirs[root as usize].files.len();
    let unresolved: u64 = files
        .iter()
        .map(|&f| g.files[f as usize].unresolved as u64)
        .sum();
    let dir = &g.dirs[root as usize];
    let across = if root == crate::graph::ROOT {
        member_links(g)
    } else {
        Vec::new()
    };

    Ok(fit(
        limits::OVERVIEW,
        &[12, 10, 8, 6, 5, 4, 3, 2],
        |limit| {
            let tree = rows_for(limit);
            Overview {
                root: g.id(NodeRef::Dir(root)),
                repo: g.repo_name.clone(),
                files: dir.file_count,
                loc: dir.loc,
                languages: languages.clone(),
                tree: tree.0,
                tree_more: tree.1,
                loose_files: loose,
                entries: entries
                    .iter()
                    .take(limit)
                    .map(|&(_, f, why)| Entry {
                        id: g.id(NodeRef::File(f)),
                        why,
                    })
                    .collect(),
                entries_more: entries.len().saturating_sub(limit),
                hubs: hubs
                    .iter()
                    .take(limit.min(6))
                    .map(|&(n, f)| Ranked {
                        id: g.id(NodeRef::File(f)),
                        n,
                    })
                    .collect(),
                libraries: libs
                    .iter()
                    .take(limit)
                    .map(|&(n, x)| Ranked {
                        id: g.externals[x as usize].id.clone(),
                        n,
                    })
                    .collect(),
                libraries_more: libs.len().saturating_sub(limit),
                findings: summary.clone(),
                unresolved_imports: unresolved,
                flows: crate::symbols::flows_under(repo, NodeRef::Dir(root), (limit / 4).max(1)),
                scip: crate::symbols::scip_overview(repo),
                across_members: across.clone(),
            }
        },
        |o| render(g, o),
    ))
}

/// An entry point where things start: an entry nothing but tests and its own submodules
/// imports (a Rust crate root its modules `use crate::…` from is still where the crate
/// starts), that is not a test, script, config, declaration, story or asset, not a barrel (an `index.ts` nobody
/// imports is unused rather than a start), and not inside a test, fixture or example tree
/// (a fixture's `package.json` main or Godot main scene starts the fixture, not the repo).
/// Test, fixture and example folders count below `scope`, the folder looked at (`ROOT` for
/// the whole repo): from inside `examples/web-app` that example's entries are entries.
/// Returns why it is one.
pub(crate) fn root_entry(g: &RepoGraph, f: FileIx, scope: DirIx) -> Option<&'static str> {
    let file = &g.files[f as usize];
    let why = g.entry_within(f, scope)?;
    let barrel = why == "entry name" && file.name.to_ascii_lowercase().starts_with("index.");
    let outside_importers = g.inn[f as usize]
        .iter()
        .filter(|d| g.files[d.file as usize].kind != FileKind::Test)
        .filter(|d| !submodule_of(g, f, d.file))
        .count();
    (listed_entry(why) && !barrel && !in_test_tree(g, f, scope) && outside_importers == 0)
        .then_some(why)
}

/// Whether `d` is a submodule of module root `f` ([`crate::findings::declares`]), not
/// another crate root next to it: a `main.rs`, `lib.rs`, `build.rs` or `bin/**` beside a
/// `lib.rs` or `main.rs` is a target of its own that imports the library.
fn submodule_of(g: &RepoGraph, f: FileIx, d: FileIx) -> bool {
    if !crate::findings::declares(g, f, d) {
        return false;
    }
    let root = &g.files[f as usize];
    if root.lang.as_deref() != Some("rust") || !matches!(root.name.as_str(), "lib.rs" | "main.rs") {
        return true;
    }
    let dir = root.path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let path = g.path(d);
    let rest = if dir.is_empty() {
        path
    } else {
        path.strip_prefix(dir)
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(path)
    };
    !(matches!(rest, "main.rs" | "lib.rs" | "build.rs") || rest.starts_with("bin/"))
}

/// Files other than tests that depend on `f`.
pub(crate) fn non_test_importers(g: &RepoGraph, f: FileIx) -> usize {
    g.inn[f as usize]
        .iter()
        .filter(|d| g.files[d.file as usize].kind != FileKind::Test)
        .count()
}

fn render(g: &RepoGraph, o: &Overview) -> String {
    let mut s = String::new();
    let langs: Vec<String> = o
        .languages
        .iter()
        .map(|l| format!("{} {}", l.lang, l.files))
        .collect();
    let name = if o.root == "repo:" {
        o.repo.clone()
    } else {
        short(&o.root)
    };
    s.push_str(&format!(
        "{name} · {} files · {} LOC · {}\n",
        o.files,
        num(o.loc),
        langs.join(", ")
    ));
    if !o.tree.is_empty() {
        s.push_str("folders (files, LOC, kind, !findings):\n");
        for r in &o.tree {
            let indent = "  ".repeat(r.level.saturating_sub(1) as usize);
            let bang = if r.findings > 0 {
                format!(" !{}", r.findings)
            } else {
                String::new()
            };
            let more = if r.more > 0 {
                format!(" [+{} subfolders]", r.more)
            } else {
                String::new()
            };
            s.push_str(&format!(
                "{indent}{} {} {} {}{bang}{more}\n",
                clip(&short(&r.id), 100),
                r.files,
                num(r.loc),
                r.kind.as_str()
            ));
        }
    }
    if o.tree_more > 0 {
        s.push_str(&format!(
            "+{} more folders in {}\n",
            o.tree_more,
            short(&o.root)
        ));
    }
    if o.loose_files > 0 {
        s.push_str(&format!(
            "+ {} files directly in {}\n",
            o.loose_files,
            short(&o.root)
        ));
    }
    if !o.across_members.is_empty() {
        let list: Vec<String> = o
            .across_members
            .iter()
            .map(|l| format!("{} → {} {}", l.from, l.to, l.n))
            .collect();
        s.push_str(&format!(
            "across members (file pairs): {}\n",
            list.join(", ")
        ));
    }
    if !o.entries.is_empty() {
        let list: Vec<String> = o
            .entries
            .iter()
            .map(|e| format!("{} ({})", clip(&short(&e.id), 100), e.why))
            .collect();
        s.push_str(&format!(
            "entry points: {}{}\n",
            list.join(", "),
            crate::budget::more(o.entries_more)
        ));
    }
    if let Some(f) = &o.flows {
        s.push_str(&crate::symbols::flows_line(f));
        s.push('\n');
    }
    if let Some(x) = &o.scip {
        s.push_str(&crate::symbols::scip_line(x));
        s.push('\n');
    }
    if !o.hubs.is_empty() {
        let list: Vec<String> = o
            .hubs
            .iter()
            .map(|h| format!("{} ←{}", clip(&short(&h.id), 100), h.n))
            .collect();
        s.push_str(&format!(
            "most depended on (←files using it, tests aside): {}\n",
            list.join(", ")
        ));
    }
    if !o.libraries.is_empty() {
        let list: Vec<String> = o
            .libraries
            .iter()
            .map(|h| format!("{} {}", short(&h.id), h.n))
            .collect();
        s.push_str(&format!(
            "libraries (files using it, tests aside): {}{}\n",
            list.join(", "),
            crate::budget::more(o.libraries_more)
        ));
    }
    if o.unresolved_imports > 0 {
        s.push_str(&format!("unresolved imports: {}\n", o.unresolved_imports));
    }
    s.push_str(&summary_line(&o.findings));
    if o.findings.total > 0 {
        s.push_str(" → `cartograph query findings`");
    }
    s.push('\n');
    if g.dir_imports > 0 {
        s.push_str(&format!(
            "note: {} package/namespace imports reach every file of their folder\n",
            g.dir_imports
        ));
    }
    s
}
