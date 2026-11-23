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
    /// The members of a workspace, to read `api:src/a.ts#f` and member-relative paths.
    pub members: crate::workspace::Members,
    /// Bridges (IPC, HTTP, packages), an overlay on the calls.
    pub bridges: crate::bridges::BridgeTable,
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
        t.bridges = crate::bridges::BridgeTable::build(s);
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
        let canonical = self
            .members
            .canonical(input, |p| self.by_file.contains_key(p))?;
        let raw = canonical.as_str();
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

/// A symbol in a file's listing: its id is the file's `id_prefix` + `name`.
#[derive(Debug, Clone, Serialize)]
pub struct ListedSymbol {
    /// Qualified name in the file (`Class.method`, `api.save`).
    pub name: String,
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

/// The members of one container (an object's functions, a class's methods) listed by name
/// only, when listing each with its lines does not fit: `api (150, pub): save, load, +148
/// more`. A member's id is the file's `id_prefix` + `<container>.<name>`. Members that
/// call sites reach keep a line of their own instead.
#[derive(Debug, Clone, Serialize)]
pub struct MemberGroup {
    pub container: String,
    /// Line of its first member.
    pub line: u32,
    /// Members of the container in this group (types and called members aside).
    pub total: usize,
    /// Of them, those exported.
    pub exported: usize,
    /// The names listed, in line order.
    pub names: Vec<String>,
    /// Members left out.
    pub more: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileSymbols {
    /// `file:<path>`.
    pub file: String,
    /// `sym:<path>#`: the id of a symbol listed here is this + its name.
    pub id_prefix: String,
    pub total: usize,
    /// Symbols listed one per line, with kind and lines.
    pub symbols: Vec<ListedSymbol>,
    /// Members listed by name only, grouped by container (only for a file).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberGroup>,
    /// Other symbols without a line of their own, listed by qualified name (only for a
    /// file).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<String>,
    /// Symbols left out: neither on a line, in `others` nor in a group (a group counts its
    /// own, `members_more` the members of groups left out).
    pub more: usize,
    /// Member groups left out, and their members.
    #[serde(skip_serializing_if = "is_zero")]
    pub groups_more: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub members_more: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryRow {
    pub kind: EntryKind,
    pub name: String,
    /// `path:line` of the declaration.
    pub at: String,
    /// The symbol (`sym:…`) or file (`file:…`) that runs, when known.
    pub target: Option<String>,
    /// An IPC handler: the sites that reach it over IPC (`invoke`, `send`), when the index
    /// has IPC bridges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_ipc: Option<usize>,
    /// An HTTP route: the call sites that reach it over HTTP, when the index has HTTP
    /// bridges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_http: Option<usize>,
}

impl EntryRow {
    fn of(e: &EntryPoint, t: &SymbolTable) -> EntryRow {
        let over = |entry: EntryKind, kind: cartograph_core::BridgeKind| {
            (e.kind == entry && t.bridges.bridges.iter().any(|b| b.kind == kind)).then(|| {
                t.bridges
                    .into_file(&e.file)
                    .filter(|b| b.kind == kind && b.dst_line == e.line)
                    .count()
            })
        };
        let over_ipc = over(EntryKind::Ipc, cartograph_core::BridgeKind::Ipc);
        let over_http = over(EntryKind::Http, cartograph_core::BridgeKind::Http);
        EntryRow {
            kind: e.kind,
            name: clip(&e.name, 120),
            at: format!("{}:{}", e.file, e.line),
            target: e.target.as_ref().map(|t| clip(t, 200)),
            over_ipc,
            over_http,
        }
    }

    fn text(&self) -> String {
        let target = self
            .target
            .as_deref()
            .map_or(String::new(), |t| format!(" → {}", short_target(t)));
        let over = match (self.over_ipc, self.over_http) {
            (Some(0), _) => " · no caller over IPC".to_string(),
            (Some(n), _) => format!(" · {n} over IPC"),
            (_, Some(0)) => " · no caller over HTTP".to_string(),
            (_, Some(n)) => format!(" · {n} over HTTP"),
            _ => String::new(),
        };
        format!(
            "{} {}{target} ({}){over}",
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
    pick(t, list, n).into_iter().map(|i| row(t, i)).collect()
}

/// The `n` most telling of `list` (exported, most called, types), in line order.
fn pick(t: &SymbolTable, list: &[usize], n: usize) -> Vec<usize> {
    let mut v: Vec<(usize, usize, String)> = list
        .iter()
        .map(|&i| {
            let id = t.symbols[i].id();
            (i, t.callers_of(&id).len(), id)
        })
        .collect();
    if v.len() > n {
        let key = |(i, callers, _): &(usize, usize, String)| {
            let x = &t.symbols[*i];
            (x.exported, *callers, x.kind.is_type())
        };
        v.sort_by(|a, b| {
            key(b)
                .cmp(&key(a))
                .then(t.symbols[a.0].line.cmp(&t.symbols[b.0].line))
        });
        v.truncate(n);
    }
    v.sort_by(|a, b| {
        t.symbols[a.0]
            .line
            .cmp(&t.symbols[b.0].line)
            .then(a.2.cmp(&b.2))
    });
    v.into_iter().map(|x| x.0).collect()
}

fn listed(t: &SymbolTable, i: usize) -> ListedSymbol {
    let r = row(t, i);
    ListedSymbol {
        name: t.symbols[i].qualified(),
        kind: r.kind,
        line: r.line,
        end_line: r.end_line,
        exported: r.exported,
        callers: r.callers,
        source: r.source,
    }
}

fn sym_text(r: &SymRow) -> String {
    let name = split_symbol_id(&r.id).map_or(r.id.as_str(), |(_, n)| n);
    listed_text(&ListedSymbol {
        name: name.to_string(),
        kind: r.kind,
        line: r.line,
        end_line: r.end_line,
        exported: r.exported,
        callers: r.callers,
        source: r.source,
    })
}

fn listed_text(r: &ListedSymbol) -> String {
    let lines = if r.end_line > r.line {
        format!("L{}-{}", r.line, r.end_line)
    } else {
        format!("L{}", r.line)
    };
    format!(
        "{} {} {lines}{}{}{}",
        r.kind.as_str(),
        clip(&r.name, 100),
        if r.exported { " pub" } else { "" },
        if r.callers > 0 {
            format!(" ←{}", r.callers)
        } else {
            String::new()
        },
        r.source.map_or(String::new(), |s| format!(" ({s})"))
    )
}

/// Names for a text line, each clipped.
fn names_text(names: &[String]) -> String {
    names
        .iter()
        .map(|n| clip(n, 100))
        .collect::<Vec<_>>()
        .join(", ")
}

fn group_text(g: &MemberGroup) -> String {
    let mut names = names_text(&g.names);
    if g.more > 0 {
        names.push_str(&format!(", +{} more", g.more));
    }
    let exported = match g.exported {
        0 => String::new(),
        n if n == g.total => ", pub".to_string(),
        n => format!(", {n} pub"),
    };
    format!(
        "{} ({}{exported}): {names}",
        clip(&g.container, 100),
        g.total
    )
}

/// How a file is listed: containers with at least `group_min` members (not types, not
/// called) list them by name only, at most `names` per container and at most `groups`
/// containers (the biggest); at most `rows` other symbols get a line each, and at most
/// `names` of the rest are listed by name.
#[derive(Debug, Clone, Copy)]
struct Plan {
    group_min: usize,
    rows: usize,
    names: usize,
    groups: usize,
}

const ALL: usize = usize::MAX;

impl Plan {
    const fn rows(rows: usize) -> Plan {
        Plan {
            group_min: ALL,
            rows,
            names: 0,
            groups: 0,
        }
    }

    /// Every container with at least `group_min` members by name, every other symbol on a
    /// line.
    const fn grouped(group_min: usize) -> Plan {
        Plan {
            group_min,
            rows: ALL,
            names: ALL,
            groups: ALL,
        }
    }

    /// At most `rows` lines, `names` names in a list, and as many groups as lines.
    const fn cut(rows: usize, names: usize) -> Plan {
        Plan {
            group_min: 2,
            rows,
            names,
            groups: rows,
        }
    }
}

/// Plans tried for one file, most detailed first: every symbol on its own line (as long as
/// that fits, nothing changes), then the members of the biggest containers as names, then
/// every container's, then fewer lines (the symbols left without one by name) and names.
const FILE_PLANS: &[Plan] = &[
    Plan::rows(ALL),
    Plan::grouped(40),
    Plan::grouped(16),
    Plan::grouped(6),
    Plan::grouped(2),
    Plan::cut(80, 400),
    Plan::cut(60, 300),
    Plan::cut(40, 200),
    Plan::cut(30, 150),
    Plan::cut(20, 100),
    Plan::cut(14, 60),
    Plan::cut(10, 40),
    Plan::cut(6, 20),
    Plan::cut(3, 10),
    Plan::cut(1, 4),
];

/// The listing of one file under `plan`.
fn file_listing(t: &SymbolTable, path: &str, list: &[usize], plan: Plan) -> FileSymbols {
    let mut members: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for &i in list {
        let x = &t.symbols[i];
        // A member that call sites reach keeps its line (and its count).
        if let Some(c) = &x.container
            && !x.kind.is_type()
            && t.callers_of(&x.id()).is_empty()
        {
            members.entry(c.as_str()).or_default().push(i);
        }
    }
    members.retain(|_, v| v.len() >= plan.group_min);
    // At most `plan.groups`: the biggest, then the earliest.
    let mut members: Vec<(&str, Vec<usize>)> = members.into_iter().collect();
    members.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.1[0].cmp(&b.1[0])));
    let dropped = members.split_off(plan.groups.min(members.len()));
    let dropped_members: HashSet<usize> =
        dropped.iter().flat_map(|x| x.1.iter()).copied().collect();
    let grouped: HashSet<usize> = members.iter().flat_map(|x| x.1.iter()).copied().collect();
    let rest: Vec<usize> = list
        .iter()
        .copied()
        .filter(|i| !grouped.contains(i) && !dropped_members.contains(i))
        .collect();
    let rows = pick(t, &rest, plan.rows);
    let shown: HashSet<usize> = rows.iter().copied().collect();
    let unlisted: Vec<usize> = rest
        .iter()
        .copied()
        .filter(|i| !shown.contains(i))
        .collect();
    let others: Vec<String> = pick(t, &unlisted, plan.names)
        .into_iter()
        .map(|i| t.symbols[i].qualified())
        .collect();
    let mut groups: Vec<MemberGroup> = members
        .into_iter()
        .map(|(c, v)| {
            let names: Vec<String> = pick(t, &v, plan.names)
                .into_iter()
                .map(|i| t.symbols[i].name.clone())
                .collect();
            MemberGroup {
                container: c.to_string(),
                line: v.iter().map(|&i| t.symbols[i].line).min().unwrap_or(0),
                total: v.len(),
                exported: v.iter().filter(|&&i| t.symbols[i].exported).count(),
                more: v.len() - names.len(),
                names,
            }
        })
        .collect();
    groups.sort_by(|a, b| a.line.cmp(&b.line).then(a.container.cmp(&b.container)));
    FileSymbols {
        file: format!("file:{}", clip(path, 240)),
        id_prefix: format!("sym:{path}#"),
        total: list.len(),
        more: unlisted.len() - others.len(),
        symbols: rows.into_iter().map(|i| listed(t, i)).collect(),
        members: groups,
        others,
        groups_more: dropped.len(),
        members_more: dropped_members.len(),
    }
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
    // A file: [`FILE_PLANS`]. A folder: the files with most call sites reaching them, a few
    // symbols each.
    let steps: Vec<(usize, Plan)> = if single {
        FILE_PLANS.iter().map(|&p| (1, p)).collect()
    } else {
        [
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
        .into_iter()
        .map(|(files, per)| (files, Plan::rows(per)))
        .collect()
    };
    let idx: Vec<usize> = (0..steps.len()).collect();
    Ok(fit(
        limits::SYMBOLS,
        &idx,
        |k| {
            let (max_files, plan) = steps[k.min(steps.len() - 1)];
            let max_entries = plan.rows.saturating_mul(2).clamp(2, 12);
            SymbolsResult {
                node: g.id(n),
                files: with.len(),
                total,
                exported,
                calls_out: counts.clone(),
                groups: with
                    .iter()
                    .take(max_files)
                    .map(|&(_, path, list)| file_listing(t, path, list, plan))
                    .collect(),
                groups_more: with.len().saturating_sub(max_files),
                entries: entries
                    .iter()
                    .take(max_entries)
                    .map(|e| EntryRow::of(e, t))
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
        let grouped = if r.groups.iter().any(|g| !g.members.is_empty()) {
            ", X (N): a, b = the N members of X by name"
        } else {
            ""
        };
        s.push_str(&format!(
            "symbols in {where_} · {} in {} (exported {}) · pub = exported, ←N = call sites reaching it{grouped}\n",
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
        // Lines and member groups in line order (a group at its first member, under its
        // container's line when that comes right before).
        let (mut rows, mut groups) = (gr.symbols.iter().peekable(), gr.members.iter().peekable());
        let mut last: Option<&str> = None;
        loop {
            let row_first = match (rows.peek(), groups.peek()) {
                (Some(x), Some(m)) => x.line <= m.line,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if row_first && let Some(x) = rows.next() {
                let nested = if x.kind == SymbolKind::Method {
                    "  "
                } else {
                    ""
                };
                s.push_str(&format!("{indent}{nested}{}\n", listed_text(x)));
                last = Some(&x.name);
            } else if let Some(m) = groups.next() {
                let under = if last == Some(m.container.as_str()) {
                    "  "
                } else {
                    ""
                };
                s.push_str(&format!("{indent}{under}{}\n", group_text(m)));
                last = None;
            }
        }
        if gr.groups_more > 0 {
            s.push_str(&format!(
                "{indent}+{} more members of {} more containers\n",
                gr.members_more, gr.groups_more
            ));
        }
        if !gr.others.is_empty() {
            let mut names = names_text(&gr.others);
            if gr.more > 0 {
                names.push_str(&format!(", +{} more", gr.more));
            }
            s.push_str(&format!(
                "{indent}others by name ({}): {names}\n",
                gr.others.len() + gr.more
            ));
        } else if gr.more > 0 {
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
    /// The call resolved to this declaration of the method in a supertype (an interface
    /// method the symbol implements, a base method it overrides): `Type.method`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// A caller over a bridge: a site in another process or repository that reaches the
/// symbol through an IPC channel, an exposed preload function or an HTTP route.
#[derive(Debug, Clone, Serialize)]
pub struct BridgedCaller {
    pub kind: cartograph_core::BridgeKind,
    /// `invoke`, `send`, `push`, `expose`.
    pub subtype: String,
    /// The channel, route or exposed name (`docs:save`, `window.api.getUser`).
    pub key: String,
    pub file: String,
    pub line: u32,
    /// The function that makes the call there.
    pub caller: Option<String>,
    pub confidence: Confidence,
    /// One bridge further: who reaches that function (a renderer view calling the exposed
    /// preload function that invokes the channel).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reached_from: Vec<BridgedSite>,
    #[serde(skip_serializing_if = "is_zero")]
    pub reached_more: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgedSite {
    /// The key of that bridge (`window.api.getUser`).
    pub key: String,
    pub file: String,
    pub line: u32,
    pub caller: Option<String>,
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
    /// Of `total`, the calls that resolved to the method as a supertype declares it
    /// (`via` on their sites), and those declarations (`Type.method`).
    #[serde(skip_serializing_if = "is_zero")]
    pub through: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub through_decls: Vec<String>,
    /// Callers over a bridge (IPC, HTTP): see [`BridgedCaller`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bridged: Vec<BridgedCaller>,
    #[serde(skip_serializing_if = "is_zero")]
    pub bridged_more: usize,
}

/// The callers of `id` over a bridge, each with who reaches its own caller over one more
/// bridge; by file and line.
fn bridged_callers(t: &SymbolTable, id: &str) -> Vec<(BridgedCaller, Vec<BridgedSite>)> {
    let mut out: Vec<(BridgedCaller, Vec<BridgedSite>)> = t
        .bridges
        .into_symbol(id)
        .map(|b| {
            let mut reach: Vec<BridgedSite> = b
                .src
                .as_deref()
                .map(|s| {
                    t.bridges
                        .into_symbol(s)
                        .filter(|x| x.src_file != b.dst_file)
                        .map(|x| BridgedSite {
                            key: x.key.clone(),
                            file: x.src_file.clone(),
                            line: x.src_line,
                            caller: x.src.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            reach.sort_by(|a, b| (&a.key, &a.file, a.line).cmp(&(&b.key, &b.file, b.line)));
            (
                BridgedCaller {
                    kind: b.kind,
                    subtype: b.subtype.clone(),
                    key: b.key.clone(),
                    file: b.src_file.clone(),
                    line: b.src_line,
                    caller: b.src.clone(),
                    confidence: b.confidence,
                    reached_from: Vec::new(),
                    reached_more: 0,
                },
                reach,
            )
        })
        .collect();
    out.sort_by(|a, b| (&a.0.file, a.0.line).cmp(&(&b.0.file, b.0.line)));
    out
}

/// A call site of `callers`, with the supertype declaration it went through (`Type.m`).
type Site<'t> = (&'t SymbolCall, Option<&'t str>);

/// Hops of supertypes `callers` looks through (as `hierarchy`).
const SUPER_HOPS: usize = 5;

/// Method `id` (`sym:path#Type.m`) as the supertypes of its type declare it, nearest
/// first: `(id of that declaration, Type.m)`. Every type once, up to [`SUPER_HOPS`] hops.
/// A call through an interface or a base class (`service.m()` on a field typed with the
/// interface) resolves to that declaration and may run this method.
fn super_decls(t: &SymbolTable, id: &str) -> Vec<(String, String)> {
    let Some((path, q)) = split_symbol_id(id) else {
        return Vec::new();
    };
    let Some((ty, m)) = q.rsplit_once('.') else {
        return Vec::new();
    };
    // The method's type: its id carries only the innermost name (`Inner.m` of a type
    // `Outer.Inner`), and a Rust `impl` or a C++ out-of-line definition may sit in another
    // file than the type. A type of that name in the method's file, else the only one in
    // the repo.
    let is_type = |i: &usize| t.symbols[*i].kind.is_type() && t.symbols[*i].name == ty;
    let mut level: Vec<String> = t
        .of_file(path)
        .iter()
        .filter(|i| is_type(i))
        .map(|&i| t.symbols[i].id())
        .collect();
    if level.is_empty()
        && let [one] = t
            .by_name
            .get(ty)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|i| is_type(i))
            .collect::<Vec<_>>()
            .as_slice()
    {
        level.push(t.symbols[**one].id());
    }
    let mut seen: HashSet<String> = level.iter().cloned().collect();
    let mut out = Vec::new();
    for _ in 0..SUPER_HOPS {
        let mut next = Vec::new();
        for cur in &level {
            for e in t.supers_of(cur) {
                let Some(x) = &e.target else { continue };
                if !seen.insert(x.clone()) {
                    continue;
                }
                if let Some((xp, xn)) = split_symbol_id(x) {
                    let leaf = xn.rsplit('.').next().unwrap_or(xn);
                    let decl = format!("sym:{xp}#{leaf}.{m}");
                    if decl != id && t.symbol(&decl).is_some() {
                        out.push((decl, format!("{leaf}.{m}")));
                    }
                }
                next.push(x.clone());
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    out
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
    // Direct calls, then calls to the method as a supertype declares it.
    let decls = if def.kind == SymbolKind::Method {
        super_decls(t, &id)
    } else {
        Vec::new()
    };
    let mut calls: Vec<Site<'_>> = t
        .callers_of(&id)
        .iter()
        .map(|&i| (&t.calls[i], None))
        .collect();
    let mut through_decls = Vec::new();
    // A method of the symbol's own type calling the supertype's declaration does it on
    // purpose (`super.m()` in the override): not a call that may run the override.
    let own = split_symbol_id(&id)
        .and_then(|(p, q)| q.rsplit_once('.').map(|(ty, _)| format!("sym:{p}#{ty}.")));
    for (d, name) in &decls {
        let before = calls.len();
        calls.extend(
            t.callers_of(d)
                .iter()
                .map(|&i| (&t.calls[i], Some(name.as_str())))
                .filter(|(c, _)| {
                    !own.as_ref().is_some_and(|o| {
                        c.caller
                            .as_deref()
                            .is_some_and(|x| x.starts_with(o.as_str()))
                    })
                }),
        );
        if calls.len() > before {
            through_decls.push(clip(name, 120));
        }
    }
    let through = calls.iter().filter(|c| c.1.is_some()).count();
    let bridged = bridged_callers(t, &id);
    let mut by_file: BTreeMap<&str, Vec<Site<'_>>> = BTreeMap::new();
    for c in &calls {
        by_file.entry(c.0.file.as_str()).or_default().push(*c);
    }
    // A symbol of a test or fixture tree is called from there: nothing to set apart.
    let g = &repo.graph;
    let in_tests = |path: &str| {
        g.file_by_path(path)
            .is_some_and(|f| in_test_tree(g, f, ROOT))
    };
    let split_tests = !in_tests(&def.file);
    let mut groups: Vec<(&str, bool, Vec<Site<'_>>)> = by_file
        .into_iter()
        .map(|(file, cs)| (file, split_tests && in_tests(file), cs))
        .collect();
    groups.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(b.2.len().cmp(&a.2.len()))
            .then(a.0.cmp(b.0))
    });
    for g in &mut groups {
        g.2.sort_by_key(|c| (c.0.line, c.0.caller.clone()));
    }
    let test_files = groups.iter().filter(|g| g.1).count();
    let test_total: usize = groups.iter().filter(|g| g.1).map(|g| g.2.len()).sum();
    // Sites of test callers only when nothing else calls the symbol.
    let test_sites = test_files == groups.len();
    let count = |c: Confidence| calls.iter().filter(|x| x.0.confidence == c).count();
    let scip = calls.iter().filter(|x| t.from_scip(x.0)).count();
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
                                .map(|&(c, via)| CallSiteRow {
                                    line: c.line,
                                    caller: c.caller.as_ref().map(|x| clip(x, 240)),
                                    confidence: c.confidence,
                                    scip: t.from_scip(c),
                                    via: via.map(|v| clip(v, 120)),
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
                through,
                through_decls: through_decls.clone(),
                bridged: bridged
                    .iter()
                    .take(max_files)
                    .map(|(b, reach)| BridgedCaller {
                        reached_from: reach.iter().take(per * 2).cloned().collect(),
                        reached_more: reach.len().saturating_sub(per * 2),
                        ..b.clone()
                    })
                    .collect(),
                bridged_more: bridged.len().saturating_sub(max_files),
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
        render_bridged(r, &mut s);
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
    let through = match r.through {
        0 => String::new(),
        n if n == r.total => format!(", all through {}", r.through_decls.join(", ")),
        n => format!(", {n} of them through {}", r.through_decls.join(", ")),
    };
    s.push_str(&format!(
        " · {} call site{} in {} ({}){through}\n",
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
            let via = c
                .via
                .as_deref()
                .map_or(String::new(), |v| format!(", via {v}"));
            s.push_str(&format!(
                "{indent}  :{} {who} [{}{scip}{via}]\n",
                c.line, c.confidence
            ));
        }
        if gr.more > 0 {
            s.push_str(&format!("{indent}  +{} more\n", gr.more));
        }
    }
    render_bridged(r, &mut s);
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

/// The callers over a bridge: `over IPC "docs:save" (invoke) from path:line f()`, then
/// who reaches that caller.
fn render_bridged(r: &CallersResult, s: &mut String) {
    if r.bridged.is_empty() {
        return;
    }
    let who = |c: &Option<String>| {
        c.as_deref().map_or(String::new(), |x| {
            split_symbol_id(x).map_or(String::new(), |(_, n)| format!(" {}()", clip(n, 80)))
        })
    };
    for b in &r.bridged {
        s.push_str(&format!(
            "over {} \"{}\" ({}) from {}:{}{} [{}]\n",
            crate::bridges::kind_label(b.kind),
            clip(&b.key, 120),
            b.subtype,
            clip(&b.file, 200),
            b.line,
            who(&b.caller),
            b.confidence
        ));
        let mut last_key = "";
        for x in &b.reached_from {
            if x.key != last_key {
                s.push_str(&format!("  reached from {} by:\n", clip(&x.key, 120)));
                last_key = &x.key;
            }
            s.push_str(&format!(
                "    {}:{}{}\n",
                clip(&x.file, 200),
                x.line,
                who(&x.caller)
            ));
        }
        if b.reached_more > 0 {
            s.push_str(&format!("    +{} more\n", b.reached_more));
        }
    }
    if r.bridged_more > 0 {
        s.push_str(&format!("+{} more over bridges\n", r.bridged_more));
    }
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
            items: list.iter().take(n).map(|e| EntryRow::of(e, t)).collect(),
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
        first: list.iter().take(n).map(|e| EntryRow::of(e, t)).collect(),
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
