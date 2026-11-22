//! The symbol layer (N2): `symbols(file|dir)`, `callers(sym)` and `entries()`, the symbol
//! resolver they share (`sym:path#name`, `path#name`, `Class.method`, a bare name), and
//! what `explain` and `overview` show of it.
//!
//! Call data never reaches the file graph: it lives here, next to [`crate::RepoGraph`].

use crate::budget::{Answer, clip, fit, limits};
use crate::graph::{NodeRef, ROOT};
use crate::test_trees::in_test_tree;
use crate::text::short;
use crate::{QueryError, Repo};
use cartograph_core::{
    CallStats, Confidence, EntryKind, EntryPoint, SuperType, Symbol, SymbolCall, SymbolKind,
    Symbols, split_path_name, split_symbol_id,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Symbols, calls and entry points, indexed for queries.
#[derive(Debug, Default)]
pub struct SymbolTable {
    pub symbols: Vec<Symbol>,
    pub calls: Vec<SymbolCall>,
    pub entries: Vec<EntryPoint>,
    /// Supertype edges (see [`SuperType`]).
    pub supers: Vec<SuperType>,
    /// Whether the index has a symbol layer at all (an index written before it has none).
    pub indexed: bool,
    stats: HashMap<String, CallStats>,
    by_file: HashMap<String, Vec<usize>>,
    by_id: HashMap<String, Vec<usize>>,
    /// Name and qualified name → symbols.
    by_name: HashMap<String, Vec<usize>>,
    /// Target id → calls.
    callers: HashMap<String, Vec<usize>>,
    /// Subtype id → its supertype edges, in stored order.
    supers_of: HashMap<String, Vec<usize>>,
    /// Supertype id → the edges that name it.
    subs_of: HashMap<String, Vec<usize>>,
    /// Files whose calls come from an imported SCIP index (N3).
    scip_files: HashSet<String>,
    /// Symbols added from SCIP definitions, as `(id, line)`.
    scip_added: HashSet<(String, u32)>,
    /// Counts of the last SCIP import, when there was one.
    pub scip: Option<cartograph_core::ScipStats>,
}

impl SymbolTable {
    pub fn build(s: &Symbols) -> SymbolTable {
        let mut t = SymbolTable {
            symbols: s.symbols.clone(),
            calls: s.calls.clone(),
            entries: s.entries.clone(),
            supers: s.supers.clone(),
            // The indexer marks the layer; an index written before the mark has rows.
            indexed: s.indexed
                || !(s.symbols.is_empty() && s.stats.is_empty() && s.entries.is_empty()),
            ..SymbolTable::default()
        };
        for (i, x) in t.symbols.iter().enumerate() {
            t.by_file.entry(x.file.clone()).or_default().push(i);
            t.by_id.entry(x.id()).or_default().push(i);
            t.by_name.entry(x.name.clone()).or_default().push(i);
            if x.container.is_some() {
                t.by_name.entry(x.qualified()).or_default().push(i);
            }
        }
        for (i, c) in t.calls.iter().enumerate() {
            t.callers.entry(c.target.clone()).or_default().push(i);
        }
        for st in &s.stats {
            t.stats.insert(st.file.clone(), st.clone());
        }
        for (i, x) in t.supers.iter().enumerate() {
            t.supers_of.entry(x.sub.clone()).or_default().push(i);
            if let Some(target) = &x.target {
                t.subs_of.entry(target.clone()).or_default().push(i);
            }
        }
        t.scip_files = s.scip.files.iter().cloned().collect();
        t.scip_added = s
            .scip
            .added
            .iter()
            .map(|x| (x.id.clone(), x.line))
            .collect();
        t.scip = s.scip.stats.clone();
        t
    }

    /// Supertype edges of type id `id` (a symbol, or `file:` for a GDScript script).
    pub fn supers_of(&self, id: &str) -> impl Iterator<Item = &SuperType> {
        self.supers_of
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.supers[i])
    }

    /// Edges whose supertype is type id `id`: its direct subtypes.
    pub fn subs_of(&self, id: &str) -> impl Iterator<Item = &SuperType> {
        self.subs_of
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.supers[i])
    }

    /// Whether type id `id` has supertypes or subtypes in the index.
    pub fn in_hierarchy(&self, id: &str) -> bool {
        self.supers_of.contains_key(id) || self.subs_of.contains_key(id)
    }

    /// The first symbol with id `id`.
    pub fn symbol(&self, id: &str) -> Option<&Symbol> {
        self.first_of(id)
    }

    /// Whether an imported SCIP index covers the file at `path`.
    pub fn scip_covers(&self, path: &str) -> bool {
        self.scip_files.contains(path)
    }

    /// Whether the call comes from SCIP: an exact call of a covered file (a covered file
    /// keeps the calls N2 found to symbols the index does not define, at their confidence).
    pub fn from_scip(&self, c: &SymbolCall) -> bool {
        c.confidence == Confidence::Exact && self.scip_covers(&c.file)
    }

    /// Symbols of a file, in line order.
    pub fn of_file(&self, path: &str) -> &[usize] {
        self.by_file.get(path).map_or(&[], Vec::as_slice)
    }

    /// Calls that reach symbol id `id`.
    pub fn callers_of(&self, id: &str) -> &[usize] {
        self.callers.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn stats_of(&self, path: &str) -> Option<&CallStats> {
        self.stats.get(path)
    }

    fn first_of(&self, id: &str) -> Option<&Symbol> {
        self.by_id
            .get(id)
            .and_then(|v| v.first())
            .map(|&i| &self.symbols[i])
    }

    /// Resolve what a user or agent typed into one symbol id: `sym:<path>#<name>`,
    /// `<path>#<name>`, `Class.method` or a bare name. Several symbols with that name are
    /// an [`QueryError::Ambiguous`] listing them; none is an unknown node with the closest
    /// names.
    pub fn resolve(&self, input: &str) -> Result<String, QueryError> {
        let raw = input.trim();
        if !self.indexed {
            return Err(QueryError::BadArgument(
                "this index has no symbols; re-run `cartograph index`".into(),
            ));
        }
        let s = raw.strip_prefix("sym:").unwrap_or(raw).replace('\\', "/");
        let s = s.strip_prefix("./").unwrap_or(&s);
        let s = s.strip_prefix("file:").unwrap_or(s);
        // A whole id, then a name that holds a `#` (`Model.#p`), then `path#name`.
        let whole = format!("sym:{s}");
        if self.by_id.contains_key(&whole) {
            return Ok(whole);
        }
        if let Some((path, name)) = self.split_ref(s) {
            let id = format!("sym:{path}#{name}");
            if self.by_id.contains_key(&id) {
                return Ok(id);
            }
            // The name alone, within that file, then anywhere.
            let in_file: Vec<String> = self
                .of_file(path)
                .iter()
                .map(|&i| self.symbols[i].id())
                .filter(|x| x.ends_with(&format!("#{name}")) || x.ends_with(&format!(".{name}")))
                .collect();
            let mut in_file = dedup(in_file);
            return match in_file.len() {
                1 => Ok(in_file.remove(0)),
                0 => Err(QueryError::UnknownNode {
                    input: raw.to_string(),
                    suggestions: self.suggest(name, Some(path)),
                }),
                _ => Err(self.ambiguous(raw, in_file)),
            };
        }
        let ids = dedup(
            self.by_name
                .get(s)
                .into_iter()
                .flatten()
                .map(|&i| self.symbols[i].id())
                .collect(),
        );
        match ids.len() {
            1 => Ok(ids.into_iter().next().unwrap_or_default()),
            0 => Err(QueryError::UnknownNode {
                input: raw.to_string(),
                suggestions: self.suggest(s, None),
            }),
            _ => Err(self.ambiguous(raw, ids)),
        }
    }

    /// Several symbols fit `input`: the five most called lead the candidates.
    fn ambiguous(&self, input: &str, mut ids: Vec<String>) -> QueryError {
        ids.sort_by(|a, b| {
            self.callers_of(b)
                .len()
                .cmp(&self.callers_of(a).len())
                .then(a.cmp(b))
        });
        QueryError::Ambiguous {
            input: input.to_string(),
            count: ids.len(),
            candidates: ids.into_iter().take(5).collect(),
        }
    }

    /// `path#name` typed by a user: split at the first `#` whose path is a file with
    /// symbols (a `#` can be in the path and in the name), else by the id rule
    /// ([`split_path_name`]). A name with a `#` and no path (`Model.#p`) is not split.
    fn split_ref<'s>(&self, s: &'s str) -> Option<(&'s str, &'s str)> {
        if self.by_name.contains_key(s) {
            return None;
        }
        s.match_indices('#')
            .map(|(i, _)| (&s[..i], &s[i + 1..]))
            .find(|(path, _)| self.by_file.contains_key(*path))
            // No path ends in `.`: `Model.#q` is a name, not `Model.` + `q`.
            .or_else(|| split_path_name(s).filter(|(p, _)| !p.ends_with('.')))
    }

    /// Closest symbol ids to `name` (in `file` first).
    fn suggest(&self, name: &str, file: Option<&str>) -> Vec<String> {
        let want = name.to_lowercase();
        let mut scored: Vec<(f64, String)> = Vec::new();
        for x in &self.symbols {
            let q = x.qualified().to_lowercase();
            let n = x.name.to_lowercase();
            let mut s = if n == want || q == want {
                3.0
            } else if n.contains(&want) || want.contains(&n) && n.len() > 2 {
                2.0
            } else {
                dice(&want, &n).max(dice(&want, &q))
            };
            if file.is_some_and(|f| f == x.file) {
                s += 1.0;
            }
            if s >= 0.5 {
                scored.push((s, x.id()));
            }
        }
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
        dedup(scored.into_iter().map(|x| x.1).collect())
            .into_iter()
            .take(3)
            .collect()
    }
}

/// Symbols a partial name fits ([`SymbolTable::partial`]).
#[derive(Debug, PartialEq)]
pub enum Partial {
    One(String),
    Many(Vec<String>),
    None,
}

/// Shortest partial name [`SymbolTable::partial`] completes.
const PARTIAL_MIN_CHARS: usize = 3;

impl SymbolTable {
    /// The symbols whose name or `Class.method` starts with what was typed (case
    /// insensitive), else those that contain it, for a name no symbol has: `formatAction`
    /// → `formatActionCommand`. `path#part` looks in that file only. Fewer than
    /// [`PARTIAL_MIN_CHARS`] characters complete nothing; neither does a fuzzier match (a
    /// typo): those stay suggestions of the unknown-name error.
    pub fn partial(&self, input: &str) -> Partial {
        let raw = input.trim();
        let s = raw.strip_prefix("sym:").unwrap_or(raw).replace('\\', "/");
        let s = s.strip_prefix("./").unwrap_or(&s);
        let s = s.strip_prefix("file:").unwrap_or(s);
        let (scope, name): (Vec<usize>, &str) = match self.split_ref(s) {
            Some((path, name)) => (self.of_file(path).to_vec(), name),
            None => ((0..self.symbols.len()).collect(), s),
        };
        let want = name.to_lowercase();
        if want.chars().count() < PARTIAL_MIN_CHARS {
            return Partial::None;
        }
        let (mut prefix, mut inside) = (Vec::new(), Vec::new());
        for i in scope {
            let x = &self.symbols[i];
            let n = x.name.to_lowercase();
            let q = x.qualified().to_lowercase();
            if n.starts_with(&want) || q.starts_with(&want) {
                prefix.push(x.id());
            } else if n.contains(&want) || q.contains(&want) {
                inside.push(x.id());
            }
        }
        let mut ids = dedup(if prefix.is_empty() { inside } else { prefix });
        match ids.len() {
            0 => Partial::None,
            1 => Partial::One(ids.remove(0)),
            _ => Partial::Many(ids),
        }
    }
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

/// Dice coefficient over character pairs.
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

/// ` · precise (scip)` for a file, ` · precise (scip) in N files` for a folder.
fn scip_note(files: usize, single: bool) -> String {
    if single {
        " · precise (scip)".to_string()
    } else {
        format!(" · precise (scip) in {}", crate::text::files(files))
    }
}

/// A symbol in a listing.
#[derive(Debug, Clone, Serialize)]
pub struct SymRow {
    pub id: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub end_line: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub exported: bool,
    /// Call sites that reach it.
    pub callers: usize,
    /// `scip` for a definition only an imported SCIP index found (N2 did not).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
}

/// Calls made from a file or folder, by outcome.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CallCounts {
    pub exact: u64,
    pub resolved: u64,
    pub heuristic: u64,
    pub ambiguous: u64,
    pub unresolved: u64,
}

impl CallCounts {
    fn add(&mut self, s: &CallStats) {
        self.exact += u64::from(s.exact);
        self.resolved += u64::from(s.resolved);
        self.heuristic += u64::from(s.heuristic);
        self.ambiguous += u64::from(s.ambiguous);
        self.unresolved += u64::from(s.unresolved);
    }

    fn total(&self) -> u64 {
        self.exact + self.resolved + self.heuristic + self.ambiguous + self.unresolved
    }

    fn line(&self) -> String {
        format!(
            "calls made here: {} resolved (exact {}, resolved {}, heuristic {}), ambiguous {}, unresolved {}",
            self.exact + self.resolved + self.heuristic,
            self.exact,
            self.resolved,
            self.heuristic,
            self.ambiguous,
            self.unresolved
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileSymbols {
    pub file: String,
    pub total: usize,
    pub symbols: Vec<SymRow>,
    pub more: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryRow {
    pub kind: EntryKind,
    pub name: String,
    /// `path:line` of the declaration.
    pub at: String,
    /// The symbol (`sym:…`) or file (`file:…`) that runs, when known.
    pub target: Option<String>,
}

impl EntryRow {
    fn of(e: &EntryPoint) -> EntryRow {
        EntryRow {
            kind: e.kind,
            name: clip(&e.name, 120),
            at: format!("{}:{}", e.file, e.line),
            target: e.target.as_ref().map(|t| clip(t, 200)),
        }
    }

    fn text(&self) -> String {
        let target = self
            .target
            .as_deref()
            .map_or(String::new(), |t| format!(" → {}", short_target(t)));
        format!(
            "{} {}{target} ({})",
            self.kind.as_str(),
            self.name,
            clip(&self.at, 120)
        )
    }
}

/// `sym:a/b.ts#run` → `run`; `file:a/b.ts` → `a/b.ts`.
fn short_target(t: &str) -> String {
    match split_symbol_id(t) {
        Some((_, name)) => clip(name, 80),
        None => clip(&short(t), 120),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolsResult {
    pub node: String,
    /// Files with symbols.
    pub files: usize,
    pub total: usize,
    pub exported: usize,
    pub calls_out: CallCounts,
    pub groups: Vec<FileSymbols>,
    pub groups_more: usize,
    pub entries: Vec<EntryRow>,
    pub entries_more: usize,
    /// Files here whose calls come from an imported SCIP index (all exact).
    #[serde(skip_serializing_if = "is_zero")]
    pub scip_files: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn row(t: &SymbolTable, i: usize) -> SymRow {
    let x = &t.symbols[i];
    let id = x.id();
    SymRow {
        source: t
            .scip_added
            .contains(&(id.clone(), x.line))
            .then_some("scip"),
        callers: t.callers_of(&id).len(),
        id: clip(&id, 240),
        kind: x.kind,
        line: x.line,
        end_line: x.end_line,
        exported: x.exported,
    }
}

/// The `n` most telling symbols of a list (exported, most called, types), in line order.
fn top_rows(t: &SymbolTable, list: &[usize], n: usize) -> Vec<SymRow> {
    let mut v: Vec<SymRow> = list.iter().map(|&i| row(t, i)).collect();
    if v.len() > n {
        v.sort_by(|a, b| {
            (b.exported, b.callers, b.kind.is_type())
                .cmp(&(a.exported, a.callers, a.kind.is_type()))
                .then(a.line.cmp(&b.line))
        });
        v.truncate(n);
    }
    v.sort_by(|a, b| a.line.cmp(&b.line).then(a.id.cmp(&b.id)));
    v
}

fn sym_text(r: &SymRow) -> String {
    let name = split_symbol_id(&r.id).map_or(r.id.as_str(), |(_, n)| n);
    let lines = if r.end_line > r.line {
        format!("L{}-{}", r.line, r.end_line)
    } else {
        format!("L{}", r.line)
    };
    format!(
        "{} {} {lines}{}{}{}",
        r.kind.as_str(),
        clip(name, 100),
        if r.exported { " pub" } else { "" },
        if r.callers > 0 {
            format!(" ←{}", r.callers)
        } else {
            String::new()
        },
        r.source.map_or(String::new(), |s| format!(" ({s})"))
    )
}

/// `symbols(file|dir)`, within [`limits::SYMBOLS`]: definitions with kind, lines, whether
/// exported and how many call sites reach each; what the calls made there resolved to; flow
/// entry points declared there.
pub fn symbols(repo: &Repo, node: &str) -> Result<Answer<SymbolsResult>, QueryError> {
    let t = &repo.symbols;
    if !t.indexed {
        return Err(QueryError::BadArgument(
            "this index has no symbols; re-run `cartograph index`".into(),
        ));
    }
    let g = &repo.graph;
    let n = repo.resolve(node)?;
    let files = match n {
        NodeRef::Ext(_) => {
            return Err(QueryError::BadArgument(format!(
                "{} is a library; symbols takes a file or a folder",
                g.label(n)
            )));
        }
        _ => g.files_of(n),
    };
    let single = matches!(n, NodeRef::File(_));
    let mut with: Vec<(usize, &str, &[usize])> = Vec::new();
    let mut counts = CallCounts::default();
    let scip_files = files.iter().filter(|&&f| t.scip_covers(g.path(f))).count();
    let (mut total, mut exported) = (0usize, 0usize);
    for &f in &files {
        let path = g.path(f);
        if let Some(st) = t.stats_of(path) {
            counts.add(st);
        }
        let list = t.of_file(path);
        if list.is_empty() {
            continue;
        }
        total += list.len();
        exported += list.iter().filter(|&&i| t.symbols[i].exported).count();
        let reach: usize = list
            .iter()
            .map(|&i| t.callers_of(&t.symbols[i].id()).len())
            .sum();
        with.push((reach, path, list));
    }
    with.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let entries: Vec<&EntryPoint> = t
        .entries
        .iter()
        .filter(|e| match n {
            NodeRef::File(f) => e.file == g.path(f),
            NodeRef::Dir(d) => g.file_by_path(&e.file).is_some_and(|f| g.file_within(f, d)),
            NodeRef::Ext(_) => false,
        })
        .collect();
    let steps: &[(usize, usize)] = if single {
        &[
            (1, 80),
            (1, 60),
            (1, 40),
            (1, 30),
            (1, 20),
            (1, 14),
            (1, 10),
            (1, 6),
            (1, 3),
        ]
    } else {
        &[
            (14, 8),
            (12, 6),
            (10, 5),
            (8, 4),
            (6, 3),
            (5, 2),
            (4, 2),
            (3, 1),
            (2, 1),
            (1, 1),
        ]
    };
    let idx: Vec<usize> = (0..steps.len()).collect();
    Ok(fit(
        limits::SYMBOLS,
        &idx,
        |k| {
            let (max_files, per) = steps[k.min(steps.len() - 1)];
            let max_entries = (per * 2).clamp(2, 12);
            SymbolsResult {
                node: g.id(n),
                files: with.len(),
                total,
                exported,
                calls_out: counts.clone(),
                groups: with
                    .iter()
                    .take(max_files)
                    .map(|&(_, path, list)| {
                        let rows = top_rows(t, list, per);
                        FileSymbols {
                            file: format!("file:{}", clip(path, 240)),
                            total: list.len(),
                            more: list.len() - rows.len(),
                            symbols: rows,
                        }
                    })
                    .collect(),
                groups_more: with.len().saturating_sub(max_files),
                entries: entries
                    .iter()
                    .take(max_entries)
                    .map(|e| EntryRow::of(e))
                    .collect(),
                entries_more: entries.len().saturating_sub(max_entries),
                scip_files,
            }
        },
        render_symbols,
    ))
}

fn render_symbols(r: &SymbolsResult) -> String {
    let mut s = String::new();
    let where_ = clip(&short(&r.node), 120);
    if r.total == 0 {
        s.push_str(&format!("no symbols in {where_}\n"));
    } else {
        s.push_str(&format!(
            "symbols in {where_} · {} in {} (exported {}) · pub = exported, ←N = call sites reaching it\n",
            r.total,
            crate::text::files(r.files),
            r.exported
        ));
    }
    if r.calls_out.total() > 0 {
        s.push_str(&r.calls_out.line());
        if r.scip_files > 0 {
            s.push_str(&scip_note(r.scip_files, r.node.starts_with("file:")));
        }
        s.push('\n');
    } else if r.scip_files > 0 {
        s.push_str(scip_note(r.scip_files, r.node.starts_with("file:")).trim_start_matches(" · "));
        s.push('\n');
    }
    let single = r.groups.len() == 1 && r.groups_more == 0 && r.node.starts_with("file:");
    for gr in &r.groups {
        let indent = if single {
            ""
        } else {
            s.push_str(&format!("{} ({})\n", clip(&short(&gr.file), 120), gr.total));
            "  "
        };
        for x in &gr.symbols {
            let nested = if x.kind == SymbolKind::Method {
                "  "
            } else {
                ""
            };
            s.push_str(&format!("{indent}{nested}{}\n", sym_text(x)));
        }
        if gr.more > 0 {
            s.push_str(&format!("{indent}+{} more\n", gr.more));
        }
    }
    if r.groups_more > 0 {
        s.push_str(&format!("+{} more files\n", r.groups_more));
    }
    if !r.entries.is_empty() {
        s.push_str("flow entries:\n");
        for e in &r.entries {
            s.push_str(&format!("  {}\n", e.text()));
        }
        if r.entries_more > 0 {
            s.push_str(&format!("  +{} more\n", r.entries_more));
        }
    }
    s
}

#[derive(Debug, Clone, Serialize)]
pub struct CallSiteRow {
    pub line: u32,
    /// The calling function or method (`None` at module level).
    pub caller: Option<String>,
    pub confidence: Confidence,
    /// From an imported SCIP index (an exact call of a covered file).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub scip: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallerFile {
    pub file: String,
    pub count: usize,
    /// A test, or a file under a test, fixture or example tree (see
    /// [`CallersResult::test_total`]).
    pub test: bool,
    pub sites: Vec<CallSiteRow>,
    pub more: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallersResult {
    /// What was asked for when no symbol has that name and `symbol` is the only one whose
    /// name starts with it or contains it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instead_of: Option<String>,
    pub symbol: String,
    pub kind: SymbolKind,
    /// `path:line` of the definition.
    pub defined_at: String,
    /// Call sites.
    pub total: usize,
    pub files: usize,
    pub exact: usize,
    pub resolved: usize,
    pub heuristic: usize,
    /// Call sites and files in tests (test files and test, fixture or example trees), unless
    /// the symbol itself is defined in one. Their groups come after the others; when other
    /// callers exist they are listed without their sites.
    pub test_total: usize,
    pub test_files: usize,
    /// Calling files, those outside tests first, then by number of calls.
    pub groups: Vec<CallerFile>,
    pub groups_more: usize,
    /// Call sites from an imported SCIP index (exact).
    #[serde(skip_serializing_if = "is_zero")]
    pub scip: usize,
}

/// `callers(sym)`, within [`limits::CALLERS`]: every call site that resolved to the symbol,
/// grouped by calling file, each with the calling function and the confidence of the
/// match. Production callers come first (most calls first); callers in tests follow as a
/// summary (file and count), unless no other file calls the symbol.
///
/// A name no symbol has falls back to the symbols whose name starts with it, else contains
/// it ([`SymbolTable::partial`]): one is answered for, saying so (`no formatAction; showing
/// formatActionCommand`); several are an [`QueryError::Ambiguous`] listing them.
pub fn callers(repo: &Repo, sym: &str) -> Result<Answer<CallersResult>, QueryError> {
    let t = &repo.symbols;
    let (id, instead) = match t.resolve(sym) {
        Ok(id) => (id, None),
        Err(e @ QueryError::UnknownNode { .. }) => match t.partial(sym) {
            Partial::One(id) => (id, Some(sym.trim().to_string())),
            Partial::Many(ids) => return Err(t.ambiguous(sym.trim(), ids)),
            Partial::None => return Err(e),
        },
        Err(e) => return Err(e),
    };
    let Some(def) = t.first_of(&id) else {
        return Err(QueryError::UnknownNode {
            input: sym.to_string(),
            suggestions: Vec::new(),
        });
    };
    let calls: Vec<&SymbolCall> = t.callers_of(&id).iter().map(|&i| &t.calls[i]).collect();
    let mut by_file: BTreeMap<&str, Vec<&SymbolCall>> = BTreeMap::new();
    for c in &calls {
        by_file.entry(c.file.as_str()).or_default().push(c);
    }
    // A symbol of a test or fixture tree is called from there: nothing to set apart.
    let g = &repo.graph;
    let in_tests = |path: &str| {
        g.file_by_path(path)
            .is_some_and(|f| in_test_tree(g, f, ROOT))
    };
    let split_tests = !in_tests(&def.file);
    let mut groups: Vec<(&str, bool, Vec<&SymbolCall>)> = by_file
        .into_iter()
        .map(|(file, cs)| (file, split_tests && in_tests(file), cs))
        .collect();
    groups.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(b.2.len().cmp(&a.2.len()))
            .then(a.0.cmp(b.0))
    });
    for g in &mut groups {
        g.2.sort_by_key(|c| (c.line, c.caller.clone()));
    }
    let test_files = groups.iter().filter(|g| g.1).count();
    let test_total: usize = groups.iter().filter(|g| g.1).map(|g| g.2.len()).sum();
    // Sites of test callers only when nothing else calls the symbol.
    let test_sites = test_files == groups.len();
    let count = |c: Confidence| calls.iter().filter(|x| x.confidence == c).count();
    let scip = calls.iter().filter(|x| t.from_scip(x)).count();
    let steps: [(usize, usize); 10] = [
        (16, 8),
        (14, 6),
        (12, 5),
        (10, 4),
        (8, 3),
        (6, 3),
        (5, 2),
        (4, 2),
        (3, 1),
        (1, 1),
    ];
    let idx: Vec<usize> = (0..steps.len()).collect();
    Ok(fit(
        limits::CALLERS,
        &idx,
        |k| {
            let (max_files, per) = steps[k.min(steps.len() - 1)];
            CallersResult {
                instead_of: instead.as_deref().map(|x| clip(x, 120)),
                symbol: clip(&id, 240),
                kind: def.kind,
                defined_at: format!("{}:{}", clip(&def.file, 200), def.line),
                total: calls.len(),
                files: groups.len(),
                exact: count(Confidence::Exact),
                resolved: count(Confidence::Resolved),
                heuristic: count(Confidence::Heuristic),
                test_total,
                test_files,
                groups: groups
                    .iter()
                    .take(max_files)
                    .map(|(file, test, cs)| {
                        let per = if *test && !test_sites { 0 } else { per };
                        CallerFile {
                            file: format!("file:{}", clip(file, 240)),
                            count: cs.len(),
                            test: *test,
                            sites: cs
                                .iter()
                                .take(per)
                                .map(|c| CallSiteRow {
                                    line: c.line,
                                    caller: c.caller.as_ref().map(|x| clip(x, 240)),
                                    confidence: c.confidence,
                                    scip: t.from_scip(c),
                                })
                                .collect(),
                            more: if per == 0 {
                                0
                            } else {
                                cs.len().saturating_sub(per)
                            },
                        }
                    })
                    .collect(),
                groups_more: groups.len().saturating_sub(max_files),
                scip,
            }
        },
        render_callers,
    ))
}

fn render_callers(r: &CallersResult) -> String {
    let mut s = String::new();
    if let Some(asked) = &r.instead_of {
        let shown = split_symbol_id(&r.symbol).map_or(r.symbol.as_str(), |(_, n)| n);
        s.push_str(&format!(
            "no `{asked}`; showing `{}`, the only symbol whose name has it\n",
            clip(shown, 120)
        ));
    }
    s.push_str(&format!(
        "callers of {} ({}, {})",
        clip(&r.symbol, 160),
        r.kind.as_str(),
        clip(&r.defined_at, 160)
    ));
    if r.total == 0 {
        s.push_str(
            " · no resolved call sites (calls that did not resolve are only counted; see `symbols` of the calling folder)\n",
        );
        return s;
    }
    let mut conf = Vec::new();
    for (n, label) in [
        (r.exact, "exact"),
        (r.resolved, "resolved"),
        (r.heuristic, "heuristic"),
    ] {
        if n > 0 {
            conf.push(format!("{label} {n}"));
        }
    }
    if r.scip > 0 {
        conf.push(format!("scip {}", r.scip));
    }
    s.push_str(&format!(
        " · {} call site{} in {} ({})\n",
        r.total,
        if r.total == 1 { "" } else { "s" },
        crate::text::files(r.files),
        conf.join(", ")
    ));
    let mut tests_shown = false;
    for gr in &r.groups {
        if gr.test && !tests_shown {
            tests_shown = true;
            s.push_str(&format!(
                "in tests: {} call site{} in {}\n",
                r.test_total,
                if r.test_total == 1 { "" } else { "s" },
                crate::text::files(r.test_files)
            ));
        }
        let indent = if gr.test { "  " } else { "" };
        s.push_str(&format!(
            "{indent}{} ({})\n",
            clip(&short(&gr.file), 140),
            gr.count
        ));
        for c in &gr.sites {
            let who = c.caller.as_deref().map_or("(top level)".to_string(), |x| {
                split_symbol_id(x).map_or(x.to_string(), |(_, n)| format!("{}()", clip(n, 80)))
            });
            let scip = if c.scip { ", scip" } else { "" };
            s.push_str(&format!(
                "{indent}  :{} {who} [{}{scip}]\n",
                c.line, c.confidence
            ));
        }
        if gr.more > 0 {
            s.push_str(&format!("{indent}  +{} more\n", gr.more));
        }
    }
    if r.groups_more > 0 {
        let shown_tests = r.groups.iter().filter(|g| g.test).count();
        let more_tests = r.test_files.saturating_sub(shown_tests);
        if more_tests == 0 {
            s.push_str(&format!("+{} more files\n", r.groups_more));
        } else if more_tests == r.groups_more {
            s.push_str(&format!("+{} more files (all in tests)\n", r.groups_more));
        } else {
            s.push_str(&format!(
                "+{} more files ({more_tests} in tests)\n",
                r.groups_more
            ));
        }
    }
    s
}

#[derive(Debug, Clone, Serialize)]
pub struct EntriesResult {
    pub total: usize,
    /// Entries left out because they lie in a test, fixture or example tree below the
    /// queried scope (see `include_tests`).
    pub in_tests: usize,
    /// Entries per kind.
    pub by_kind: BTreeMap<&'static str, usize>,
    pub items: Vec<EntryRow>,
    pub more: usize,
}

/// Order of kinds in a listing.
fn kind_rank(k: EntryKind) -> u8 {
    match k {
        EntryKind::Main => 0,
        EntryKind::GodotMainScene => 1,
        EntryKind::Http => 2,
        EntryKind::Cli => 3,
        EntryKind::Ipc => 4,
        EntryKind::GodotAutoload => 5,
    }
}

/// `entries(kind?, within?, include_tests)`, within [`limits::ENTRIES`]: where flows start
/// (`main`, HTTP routes, CLI commands, IPC handlers, Godot main scenes and autoloads) with
/// the symbol
/// that runs. Like `overview`, entries in tests and in test, fixture or example trees
/// below the queried folder (the repo by default) are left out and only counted, unless
/// `include_tests`: a query within such a tree, or of one file, lists its own.
pub fn entries(
    repo: &Repo,
    kind: Option<EntryKind>,
    within: Option<&str>,
    include_tests: bool,
) -> Result<Answer<EntriesResult>, QueryError> {
    let t = &repo.symbols;
    if !t.indexed {
        return Err(QueryError::BadArgument(
            "this index has no symbols; re-run `cartograph index`".into(),
        ));
    }
    let g = &repo.graph;
    let scope = within.map(|w| repo.resolve(w)).transpose()?;
    let mut list: Vec<&EntryPoint> = t
        .entries
        .iter()
        .filter(|e| kind.is_none_or(|k| e.kind == k))
        .filter(|e| match scope {
            None => true,
            Some(NodeRef::File(f)) => e.file == g.path(f),
            Some(NodeRef::Dir(d)) => g.file_by_path(&e.file).is_some_and(|f| g.file_within(f, d)),
            Some(NodeRef::Ext(_)) => false,
        })
        .collect();
    let test_root = match scope {
        None => Some(ROOT),
        Some(NodeRef::Dir(d)) => Some(d),
        Some(NodeRef::File(_) | NodeRef::Ext(_)) => None,
    };
    let mut in_tests = 0;
    if let Some(root) = test_root.filter(|_| !include_tests) {
        list.retain(|e| {
            let keep = g
                .file_by_path(&e.file)
                .is_none_or(|f| !in_test_tree(g, f, root));
            in_tests += usize::from(!keep);
            keep
        });
    }
    list.sort_by(|a, b| {
        (kind_rank(a.kind), &a.file, a.line, &a.name).cmp(&(
            kind_rank(b.kind),
            &b.file,
            b.line,
            &b.name,
        ))
    });
    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    for e in &list {
        *by_kind.entry(e.kind.as_str()).or_default() += 1;
    }
    Ok(fit(
        limits::ENTRIES,
        &[60, 40, 30, 20, 14, 10, 6, 3, 1],
        |n| EntriesResult {
            total: list.len(),
            in_tests,
            by_kind: by_kind.clone(),
            items: list.iter().take(n).map(|e| EntryRow::of(e)).collect(),
            more: list.len().saturating_sub(n),
        },
        |r| {
            let tests = if r.in_tests > 0 {
                format!(
                    " · {} more in test, fixture or example trees (include_tests to list them)",
                    r.in_tests
                )
            } else {
                String::new()
            };
            if r.total == 0 {
                return format!(
                    "no flow entry points found (main, HTTP routes, CLI commands, IPC handlers, Godot scenes){tests}\n"
                );
            }
            let kinds: Vec<String> = r.by_kind.iter().map(|(k, n)| format!("{k} {n}")).collect();
            let mut s = format!("flow entries: {} ({}){tests}\n", r.total, kinds.join(", "));
            for e in &r.items {
                s.push_str(&format!("{}\n", e.text()));
            }
            if r.more > 0 {
                s.push_str(&format!("+{} more\n", r.more));
            }
            s
        },
    ))
}

/// What `explain` shows of a file's or folder's symbols.
#[derive(Debug, Clone, Serialize)]
pub struct ExplainSymbols {
    pub total: usize,
    pub exported: usize,
    /// The most telling ones (exported, most called), in line order (files only).
    pub main: Vec<SymRow>,
    pub more: usize,
    /// Types of the file with supertypes or subtypes (files only), most subtypes first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<TypeLinks>,
    #[serde(skip_serializing_if = "is_zero")]
    pub types_more: usize,
    /// Files here whose calls come from an imported SCIP index.
    #[serde(skip_serializing_if = "is_zero")]
    pub scip_files: usize,
}

/// A type's place in the hierarchy, as `explain` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct TypeLinks {
    /// Qualified name in its file.
    pub name: String,
    /// Supertypes as written.
    pub supers: Vec<String>,
    pub supers_more: usize,
    /// Direct subtypes in the repo.
    pub subtypes: usize,
    /// A few of them, by qualified name.
    pub subs: Vec<String>,
}

/// Types of a file that extend something or are extended, at most `n`.
fn type_links(t: &SymbolTable, file: &str, n: usize) -> (Vec<TypeLinks>, usize) {
    // A GDScript script without `class_name` is its own type.
    let script = format!("file:{file}");
    let mut ids: Vec<(String, String)> = dedup(
        t.of_file(file)
            .iter()
            .map(|&i| &t.symbols[i])
            .filter(|x| x.kind.is_type())
            .map(|x| x.id())
            .collect(),
    )
    .into_iter()
    .map(|id| {
        let name = split_symbol_id(&id).map_or(id.clone(), |(_, n)| n.to_string());
        (id, name)
    })
    .collect();
    if t.in_hierarchy(&script) {
        let name = file.rsplit('/').next().unwrap_or(file).to_string();
        ids.push((script, name));
    }
    let mut links: Vec<TypeLinks> = ids
        .into_iter()
        .filter(|(id, _)| t.in_hierarchy(id))
        .map(|(id, name)| {
            let supers: Vec<String> = t.supers_of(&id).map(|e| clip(&e.name, 60)).collect();
            let subs: Vec<String> = dedup(t.subs_of(&id).map(|e| short_target(&e.sub)).collect());
            TypeLinks {
                name: clip(&name, 100),
                supers_more: supers.len().saturating_sub(3),
                supers: supers.into_iter().take(3).collect(),
                subtypes: subs.len(),
                subs: subs.into_iter().take(3).collect(),
            }
        })
        .collect();
    links.sort_by(|a, b| b.subtypes.cmp(&a.subtypes).then(a.name.cmp(&b.name)));
    let more = links.len().saturating_sub(n);
    links.truncate(n);
    (links, more)
}

fn links_text(l: &TypeLinks) -> String {
    let mut s = l.name.clone();
    if !l.supers.is_empty() {
        s.push_str(&format!(
            " extends {}{}",
            l.supers.join(", "),
            crate::budget::more(l.supers_more)
        ));
    }
    if l.subtypes > 0 {
        s.push_str(&format!(
            "{} {} subtype{} ({}{})",
            if l.supers.is_empty() { ":" } else { ";" },
            l.subtypes,
            if l.subtypes == 1 { "" } else { "s" },
            l.subs.join(", "),
            crate::budget::more(l.subtypes.saturating_sub(l.subs.len()))
        ));
    }
    s
}

/// Symbols of the files under an `explain` node, with the `limit` main ones of a file.
pub fn for_explain(repo: &Repo, n: NodeRef, limit: usize) -> Option<ExplainSymbols> {
    let t = &repo.symbols;
    if !t.indexed {
        return None;
    }
    let g = &repo.graph;
    match n {
        NodeRef::File(f) => {
            let list = t.of_file(g.path(f));
            let main = top_rows(t, list, limit);
            let (types, types_more) = type_links(t, g.path(f), limit.min(4));
            Some(ExplainSymbols {
                total: list.len(),
                exported: list.iter().filter(|&&i| t.symbols[i].exported).count(),
                more: list.len() - main.len(),
                main,
                types,
                types_more,
                scip_files: usize::from(t.scip_covers(g.path(f))),
            })
        }
        NodeRef::Dir(_) => {
            let (mut total, mut exported, mut scip_files) = (0, 0, 0);
            for f in g.files_of(n) {
                let list = t.of_file(g.path(f));
                total += list.len();
                exported += list.iter().filter(|&&i| t.symbols[i].exported).count();
                scip_files += usize::from(t.scip_covers(g.path(f)));
            }
            Some(ExplainSymbols {
                total,
                exported,
                main: Vec::new(),
                more: 0,
                types: Vec::new(),
                types_more: 0,
                scip_files,
            })
        }
        NodeRef::Ext(_) => None,
    }
}

/// The `explain` text line for symbols.
pub fn explain_line(id: &str, s: Option<&ExplainSymbols>) -> String {
    let Some(s) = s else {
        return "symbols: not indexed (re-run `cartograph index`)".to_string();
    };
    if s.total == 0 {
        return "symbols: none".to_string();
    }
    let mut line = format!("symbols ({}, exported {})", s.total, s.exported);
    let precise = if s.scip_files > 0 {
        scip_note(s.scip_files, id.starts_with("file:"))
    } else {
        String::new()
    };
    if s.main.is_empty() {
        line.push_str(&format!(
            "{precise} → `cartograph query symbols {}`",
            short(id)
        ));
        return line;
    }
    let list: Vec<String> = s.main.iter().map(sym_text).collect();
    line.push_str(&format!(": {}", list.join(", ")));
    if s.more > 0 {
        line.push_str(&format!(" +{} more", s.more));
    }
    line.push_str(&precise);
    if !s.types.is_empty() {
        let list: Vec<String> = s.types.iter().map(links_text).collect();
        line.push_str(&format!(
            "\nhierarchy: {}{} → `cartograph query hierarchy <type>`",
            list.join(" · "),
            crate::budget::more(s.types_more)
        ));
    }
    line
}

/// What `overview` says about an imported SCIP index (N3).
#[derive(Debug, Clone, Serialize)]
pub struct ScipOverview {
    /// Files whose calls come from SCIP.
    pub files: u64,
    pub symbols_matched: u64,
    pub symbols_added: u64,
    pub calls_upgraded: u64,
    pub calls_added: u64,
    pub calls_dropped: u64,
    /// Files changed since the index was written (left to N2).
    pub stale: u64,
    /// Indexes that could not be read.
    pub errors: u64,
}

/// The SCIP summary for `overview`, when an index was imported.
pub fn scip_overview(repo: &Repo) -> Option<ScipOverview> {
    let st = repo.symbols.scip.as_ref()?;
    Some(ScipOverview {
        files: st.files_covered,
        symbols_matched: st.symbols_matched,
        symbols_added: st.symbols_added,
        calls_upgraded: st.calls_upgraded,
        calls_added: st.calls_added,
        calls_dropped: st.calls_dropped,
        stale: st.indexes.iter().map(|i| i.stale).sum(),
        errors: st.indexes.iter().filter(|i| i.error.is_some()).count() as u64,
    })
}

/// The `overview` text line for SCIP.
pub fn scip_line(o: &ScipOverview) -> String {
    let mut s = format!(
        "precise calls (scip): {} · symbols {} matched, {} added · calls {} upgraded to exact, {} added, {} dropped",
        crate::text::files(o.files as usize),
        o.symbols_matched,
        o.symbols_added,
        o.calls_upgraded,
        o.calls_added,
        o.calls_dropped
    );
    if o.stale > 0 {
        s.push_str(&format!(
            " · {} changed since (regenerate the index)",
            crate::text::files(o.stale as usize)
        ));
    }
    if o.errors > 0 {
        s.push_str(&format!(
            " · {} unreadable (see `cartograph doctor`)",
            o.errors
        ));
    }
    s
}

/// Flow entry points under a folder, by kind (for `overview`).
#[derive(Debug, Clone, Serialize)]
pub struct Flows {
    pub total: usize,
    pub by_kind: BTreeMap<&'static str, usize>,
    /// A few, `main` and routes first.
    pub first: Vec<EntryRow>,
}

/// Flow entries under `root`. Under a folder, those in tests or in test, fixture and
/// example trees below it are left out, as `overview` does for its entry files (from
/// inside `examples/x`, that example's entries count).
pub fn flows_under(repo: &Repo, root: NodeRef, n: usize) -> Option<Flows> {
    let t = &repo.symbols;
    let g = &repo.graph;
    let mut list: Vec<&EntryPoint> = t
        .entries
        .iter()
        .filter(|e| match root {
            NodeRef::Dir(d) => g
                .file_by_path(&e.file)
                .is_some_and(|f| g.file_within(f, d) && !in_test_tree(g, f, d)),
            NodeRef::File(f) => e.file == g.path(f),
            NodeRef::Ext(_) => false,
        })
        .collect();
    if list.is_empty() {
        return None;
    }
    list.sort_by(|a, b| {
        (kind_rank(a.kind), &a.file, a.line).cmp(&(kind_rank(b.kind), &b.file, b.line))
    });
    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    for e in &list {
        *by_kind.entry(e.kind.as_str()).or_default() += 1;
    }
    Some(Flows {
        total: list.len(),
        by_kind,
        first: list.iter().take(n).map(|e| EntryRow::of(e)).collect(),
    })
}

/// The `overview` text line for flows.
pub fn flows_line(f: &Flows) -> String {
    let kinds: Vec<String> = f.by_kind.iter().map(|(k, n)| format!("{k} {n}")).collect();
    let mut s = format!("flow entries: {}", kinds.join(", "));
    if !f.first.is_empty() {
        let first: Vec<String> = f.first.iter().map(EntryRow::text).collect();
        s.push_str(&format!(" · {}", first.join("; ")));
    }
    s.push_str(" → `cartograph query entries`");
    s
}
