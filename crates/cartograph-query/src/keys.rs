//! String keys (see [`cartograph_core::key`]): the trace `find` gives for a key such as an
//! action name. Where it is defined (enum constants, string constants), what runs it
//! (switch cases, registrations that bind it to code, naming annotations) with the code
//! they run, what lists it (catalogs), where it is **missing**, and its other uses, each
//! with `path:line`, across the members of a workspace.
//!
//! The query is a key as written (`READ-FILE`) or the name of a constant that defines one
//! (`READ_FILE`, `Action.READ_FILE`). Keys that differ only in case or separators
//! (`READ_FILE`, `readFile`) are named apart, as lower-confidence matches; when no key is
//! the query as written, the closest such variant is traced and labelled so (`find` lists
//! the symbols that hold the query first, when there are some).
//!
//! Missing: a container (an enum, a registry, a switch, a catalog) that holds the key's
//! siblings but not the key. Only for a key something defines (an enum or string
//! constant): the keys of its definition are its domain. The siblings are the other keys of
//! a definition, registry or switch of that domain that holds the key (its own registry, its
//! enum…); a container of that domain (most of its keys in it) that lacks the key is named
//! when it holds at least [`PEER_SHARE`] of one such group, at least [`MIN_SIBLINGS`] of
//! them, and lacks at most [`MAX_GAPS`] of them besides the key. Precision first, so never:
//! - a container of a test or a script, or with fewer than [`MIN_PEER`] keys;
//! - one in the file of the group (record layouts, required and optional lists, the
//!   schemas of one table: lists that differ by design);
//! - one whose file has the key anywhere, comments included (written another way, held
//!   by another list of that file, or left out on purpose and said so), or a variant of it;
//! - an enum or a class of constants (Java, Python): a key defined elsewhere is not
//!   missing there; a TS/JS object map can be a peer;
//! - one that lacks a family of the key's siblings with it (two or more that share a word
//!   with the key: `COPY-REMOTE` and `MOVE-REMOTE` with `OPEN-REMOTE`), a subset by
//!   design;
//! - one that shares no key with another list that holds the key (the steps without an
//!   editor are none of the editor registry's: the key is on the other side by design);
//! - for a plain lower-case word (`name`, `value`): its uses are not indexed, so the
//!   peer's file may well have it.
//!
//! At most [`MAX_MISSING`] are named, each with its evidence.

use crate::budget::{Answer, clip, fit, limits};
use crate::kind::is_test;
use crate::text::short;
use cartograph_core::{Confidence, KeyRole, KeySite, Symbols};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Fewest siblings a group needs to make a claim (the key's container holds this many
/// other keys, and the peer holds as many of them).
pub const MIN_SIBLINGS: usize = 10;
/// Most siblings a peer may lack besides the key (a peer that lacks many is a subset by
/// design, not one missing this key).
pub const MAX_GAPS: usize = 8;
/// Share of a sibling group a container must hold to be a peer (missing the key).
pub const PEER_SHARE: f64 = 0.8;
/// Fewest keys of a peer container.
pub const MIN_PEER: usize = 5;
/// Most "missing from" claims of a trace.
pub const MAX_MISSING: usize = 3;

/// Every key site of the index, by key and by the name of its constant.
#[derive(Debug, Default)]
pub struct KeyTable {
    pub sites: Vec<KeySite>,
    /// The index looked for keys (one written before them has none to trace).
    pub indexed: bool,
    by_key: HashMap<String, Vec<usize>>,
    /// Constant names (`Action.READ_FILE`, and its last part `READ_FILE`) → sites.
    by_name: HashMap<String, Vec<usize>>,
    /// [`squash`] of each key → the keys.
    by_squash: HashMap<String, BTreeSet<String>>,
    /// Key → the files whose comments name it.
    mentions: HashMap<String, BTreeSet<String>>,
}

/// Lower case, letters and digits only: `READ-FILE`, `read_file` and `readFile` are alike.
pub fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

impl KeyTable {
    pub fn build(s: &Symbols) -> KeyTable {
        let mut t = KeyTable {
            sites: s.keys.clone(),
            indexed: s.keys_indexed || !s.keys.is_empty(),
            ..KeyTable::default()
        };
        for (i, k) in t.sites.iter().enumerate() {
            t.by_key.entry(k.key.clone()).or_default().push(i);
            if let Some(n) = &k.name {
                t.by_name.entry(n.clone()).or_default().push(i);
                if let Some((_, last)) = n.rsplit_once('.') {
                    t.by_name.entry(last.to_string()).or_default().push(i);
                }
            }
            t.by_squash
                .entry(squash(&k.key))
                .or_default()
                .insert(k.key.clone());
        }
        for m in &s.mentions {
            t.mentions
                .entry(m.key.clone())
                .or_default()
                .insert(m.file.clone());
        }
        t
    }

    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    fn structural(&self, key: &str) -> usize {
        self.by_key.get(key).map_or(0, |v| {
            v.iter()
                .filter(|&&i| self.sites[i].role != KeyRole::Use)
                .count()
        })
    }

    /// The key `query` names: itself, or the key of the constant it names; `None` when no
    /// structural site (a definition, a runner, a catalog) has it.
    fn exact(&self, query: &str) -> Option<String> {
        if self.structural(query) > 0 {
            return Some(query.to_string());
        }
        let mut keys: Vec<&str> = self
            .by_name
            .get(query)
            .into_iter()
            .flatten()
            .map(|&i| self.sites[i].key.as_str())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        match keys.as_slice() {
            [one] if self.structural(one) > 0 => Some(one.to_string()),
            _ => None,
        }
    }

    /// Other spellings of `key` (case and separators ignored) with a structural site, most
    /// sites first.
    fn variants(&self, key: &str) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self
            .by_squash
            .get(&squash(key))
            .into_iter()
            .flatten()
            .filter(|k| k.as_str() != key)
            .map(|k| (k.clone(), self.structural(k)))
            .filter(|(_, n)| *n > 0)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }
}

/// One site of a trace.
#[derive(Debug, Clone, Serialize)]
pub struct TraceSite {
    pub file: String,
    pub line: u32,
    /// `enum`, `const`, `case`, `register`, `annotation`, `entry`, `use`.
    pub form: String,
    pub container: String,
    /// The constant that defines the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The function or method around it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// What it runs or binds the key to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    pub confidence: Confidence,
}

/// A container that holds the key's siblings but not the key.
#[derive(Debug, Clone, Serialize)]
pub struct Missing {
    pub container: String,
    /// Where the container is (its file and first line; the first site's when it spans
    /// files).
    pub file: String,
    pub line: u32,
    /// Of the siblings in `siblings_of`, how many it holds.
    pub holds: usize,
    pub siblings: usize,
    /// The container holding the key whose other keys are the siblings.
    pub siblings_of: String,
    pub siblings_file: String,
}

/// The trace of one key.
#[derive(Debug, Clone, Serialize)]
pub struct Trace {
    pub key: String,
    /// The key matched the query only once case and separators are ignored.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub variant: bool,
    /// The members of a workspace with a site of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    pub defined: Vec<TraceSite>,
    pub runs: Vec<TraceSite>,
    pub lists: Vec<TraceSite>,
    /// Sites of each list above left out (the budget).
    #[serde(skip_serializing_if = "is_zero")]
    pub defined_more: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub runs_more: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub lists_more: usize,
    pub missing: Vec<Missing>,
    pub uses: Vec<TraceSite>,
    /// Uses left out, and in how many files the uses are (tests among them).
    pub uses_more: usize,
    pub uses_files: usize,
    pub uses_tests: usize,
    /// Files whose comments name the key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<String>,
    /// Files with comments that name the key, left out.
    #[serde(skip_serializing_if = "is_zero")]
    pub comments_more: usize,
    /// Other spellings with a definition, runner or catalog of their own (lower
    /// confidence), with their structural sites.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<(String, usize)>,
}

fn site(k: &KeySite) -> TraceSite {
    TraceSite {
        file: k.file.clone(),
        line: k.line,
        form: k.form.clone(),
        container: k.container.clone(),
        name: k.name.clone(),
        symbol: k.symbol.clone(),
        target: k.target.clone(),
        test: is_test(&k.file),
        confidence: k.confidence,
    }
}

/// `(file, line, container)` of every container with its keys, the roles of its sites and
/// its first site, test and script files left out.
type Containers<'k> = BTreeMap<(&'k str, u32, &'k str), Container<'k>>;

/// A container's `(file, line, label)`.
type ContainerKey<'k> = (&'k str, u32, &'k str);

/// Why a peer misses the key: share of the siblings it holds, how many, of how many, and
/// the group they are the siblings in.
type Evidence<'k> = (f64, usize, usize, ContainerKey<'k>);

struct Container<'k> {
    keys: BTreeSet<&'k str>,
    roles: BTreeSet<KeyRole>,
    first: &'k KeySite,
}

impl Container<'_> {
    /// An enum or a class of constants of Java or Python: definitions only.
    fn definitions_only(&self) -> bool {
        self.roles.iter().all(|r| *r == KeyRole::Define)
            && (self.first.form == "enum"
                || self.first.file.ends_with(".java")
                || self.first.file.ends_with(".py"))
    }
}

/// The words of a key (`OPEN-REMOTE` → `open`, `remote`).
fn words(k: &str) -> Vec<String> {
    k.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// Whether a path is a test's or a script's: its lists never count for "missing".
fn aside(path: &str) -> bool {
    matches!(
        crate::kind::file_kind(path),
        crate::kind::FileKind::Test | crate::kind::FileKind::Script
    )
}

fn containers(t: &KeyTable) -> Containers<'_> {
    let mut out: Containers<'_> = BTreeMap::new();
    for k in &t.sites {
        if k.role == KeyRole::Use || aside(&k.file) {
            continue;
        }
        let c = out.entry(k.container_key()).or_insert_with(|| Container {
            keys: BTreeSet::new(),
            roles: BTreeSet::new(),
            first: k,
        });
        c.keys.insert(k.key.as_str());
        c.roles.insert(k.role);
    }
    out
}

/// Whether most keys of `c` (at least [`PEER_SHARE`]) are keys of `domain`.
fn within(c: &BTreeSet<&str>, domain: &BTreeSet<&str>) -> bool {
    c.iter().filter(|k| domain.contains(*k)).count() as f64 >= PEER_SHARE * c.len() as f64
}

/// The containers `key` is missing from (see the module docs), strongest first.
fn missing(t: &KeyTable, key: &str) -> Vec<Missing> {
    if key.chars().all(|c| c.is_ascii_lowercase()) {
        return Vec::new();
    }
    let all = containers(t);
    let sq = squash(key);
    // Files where the key (or a variant) is written at all, comments included.
    let variants = || t.by_squash.get(&sq).into_iter().flatten();
    let with_key: BTreeSet<&str> = variants()
        .flat_map(|k| t.by_key.get(k).into_iter().flatten())
        .map(|&i| t.sites[i].file.as_str())
        .chain(
            variants()
                .flat_map(|k| t.mentions.get(k).into_iter().flatten())
                .map(String::as_str),
        )
        .collect();
    let file_of = |c: &(&str, u32, &str)| -> String {
        if c.0.is_empty() {
            all.get(c).map_or(String::new(), |x| x.first.file.clone())
        } else {
            c.0.to_string()
        }
    };
    let holding: Vec<(&(&str, u32, &str), &Container<'_>)> =
        all.iter().filter(|(_, c)| c.keys.contains(key)).collect();
    // (holds / siblings, holds, siblings, group) per peer: the best group for each.
    let mut best: BTreeMap<ContainerKey<'_>, Evidence<'_>> = BTreeMap::new();
    // The key's definitions give its domain; its groups are the definitions, registries and
    // switches of that domain that hold it; its peers, the containers of that domain.
    for (_, def) in holding
        .iter()
        .filter(|(_, c)| c.roles.contains(&KeyRole::Define))
    {
        let domain = &def.keys;
        for (group, g) in holding.iter().filter(|(_, c)| {
            (c.roles.contains(&KeyRole::Define) || c.roles.contains(&KeyRole::Provide))
                && within(&c.keys, domain)
        }) {
            let siblings: Vec<&str> = g.keys.iter().copied().filter(|k| *k != key).collect();
            if siblings.len() < MIN_SIBLINGS {
                continue;
            }
            let gfile = file_of(group);
            let kwords = words(key);
            for (peer, p) in &all {
                if p.keys.len() < MIN_PEER
                    || p.keys.contains(key)
                    || p.definitions_only()
                    || !within(&p.keys, domain)
                {
                    continue;
                }
                let holds = siblings.iter().filter(|s| p.keys.contains(*s)).count();
                let share = holds as f64 / siblings.len() as f64;
                if holds < MIN_SIBLINGS || share < PEER_SHARE || siblings.len() - holds > MAX_GAPS {
                    continue;
                }
                let pfile = file_of(peer);
                if pfile == gfile
                    || with_key.contains(pfile.as_str())
                    || p.keys.iter().any(|k| squash(k) == sq)
                {
                    continue;
                }
                // The key is in a list kept apart from the peer (no key in common).
                if holding
                    .iter()
                    .any(|(_, x)| x.keys.len() >= 3 && x.keys.iter().all(|k| !p.keys.contains(k)))
                {
                    continue;
                }
                // A family left out together (`*-REMOTE`): by design.
                let family = siblings
                    .iter()
                    .filter(|s| !p.keys.contains(*s))
                    .filter(|s| words(s).iter().any(|w| kwords.contains(w)))
                    .count();
                if family >= 2 {
                    continue;
                }
                let e = best
                    .entry(*peer)
                    .or_insert((share, holds, siblings.len(), **group));
                // The highest share; on a tie the tighter group.
                if share > e.0 || (share == e.0 && siblings.len() < e.2) {
                    *e = (share, holds, siblings.len(), **group);
                }
            }
        }
    }
    let mut v: Vec<_> = best.into_iter().collect();
    v.sort_by(|a, b| {
        b.1.0
            .partial_cmp(&a.1.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.1.cmp(&a.1.1))
            .then(a.0.cmp(&b.0))
    });
    v.into_iter()
        .take(MAX_MISSING)
        .map(|(peer, (_, holds, siblings, group))| {
            let (file, line) = if peer.0.is_empty() {
                all.get(&peer)
                    .map_or((String::new(), 0), |c| (c.first.file.clone(), c.first.line))
            } else {
                (peer.0.to_string(), peer.1)
            };
            Missing {
                container: peer.2.to_string(),
                file,
                line,
                holds,
                siblings,
                siblings_of: group.2.to_string(),
                siblings_file: file_of(&group),
            }
        })
        .collect()
}

/// The key `query` names, and whether only as a variant (case or separators differ).
fn key_of(t: &KeyTable, query: &str) -> Option<(String, bool)> {
    let q = query.trim().trim_matches(|c| matches!(c, '"' | '\'' | '`'));
    match t.exact(q) {
        Some(k) => Some((k, false)),
        // A constant's name read as a key (`READ_FILE` for `READ-FILE`) or another spelling:
        // the variant with the most structural sites.
        None => t.variants(q).into_iter().next().map(|v| (v.0, true)),
    }
}

/// The trace of the key `query` names, every site; `None` when the index knows no such key
/// (as written, as a constant's name, or as a variant).
pub fn trace(repo: &crate::Repo, query: &str) -> Option<Trace> {
    let t = &repo.symbols.keys;
    if t.is_empty() {
        return None;
    }
    let (key, variant) = key_of(t, query)?;
    let idx = t.by_key.get(&key)?;
    let mut members: BTreeSet<String> = BTreeSet::new();
    let (mut defined, mut runs, mut lists, mut uses) = (vec![], vec![], vec![], vec![]);
    let comments = t.mentions.get(&key).cloned().unwrap_or_default();
    let files = idx.iter().map(|&i| &t.sites[i].file).chain(&comments);
    for f in files {
        if let Some(m) = repo.graph.members.member_of(f) {
            members.insert(m.to_string());
        }
    }
    for &i in idx {
        let k = &t.sites[i];
        match k.role {
            KeyRole::Define => defined.push(site(k)),
            KeyRole::Provide => runs.push(site(k)),
            KeyRole::List => lists.push(site(k)),
            KeyRole::Use => uses.push(site(k)),
        }
    }
    // Code first, tests last; then by path and line.
    let order = |v: &mut Vec<TraceSite>| {
        v.sort_by(|a, b| (a.test, &a.file, a.line).cmp(&(b.test, &b.file, b.line)));
    };
    for v in [&mut defined, &mut runs, &mut lists, &mut uses] {
        order(v);
    }
    let uses_files = uses.iter().map(|u| &u.file).collect::<BTreeSet<_>>().len();
    let uses_tests = uses.iter().filter(|u| u.test).count();
    let mut also = t.variants(&key);
    also.truncate(5);
    Some(Trace {
        missing: missing(t, &key),
        key,
        variant,
        members: if repo.graph.members.is_workspace() {
            members.into_iter().collect()
        } else {
            Vec::new()
        },
        defined,
        runs,
        lists,
        uses,
        uses_more: 0,
        defined_more: 0,
        runs_more: 0,
        lists_more: 0,
        uses_files,
        uses_tests,
        comments: comments.into_iter().collect(),
        comments_more: 0,
        also,
    })
}

/// `→ path (Name)` for a target or symbol id (`sym:path#Name`).
fn sym_label(id: &str) -> String {
    let id = id.strip_prefix("sym:").unwrap_or(id);
    match id.split_once('#') {
        Some((path, name)) => format!("{} ({name})", short(path)),
        None => short(id),
    }
}

fn sym_name(id: &str) -> &str {
    id.rsplit_once('#').map_or(id, |(_, n)| n)
}

fn line_of(s: &TraceSite, with_container: bool) -> String {
    let mut out = format!("  {}:{}", clip(&short(&s.file), 140), s.line);
    match s.form.as_str() {
        "enum" | "const" => {
            out.push_str(&format!(" {}", s.form));
            if let Some(n) = &s.name {
                out.push_str(&format!(" {}", clip(n, 120)));
            }
        }
        "case" => out.push_str(&format!(
            " case in {}",
            clip(&s.container, 100).trim_start_matches("switch in ")
        )),
        "register" => out.push_str(&format!(" registered in {}", clip(&s.container, 120))),
        "annotation" => out.push_str(&format!(" {}", clip(&s.container, 100))),
        "entry" => out.push_str(&format!(" {}", clip(&s.container, 120))),
        _ => {
            if with_container && let Some(sym) = &s.symbol {
                out.push_str(&format!(" in {}", clip(sym_name(sym), 100)));
            }
        }
    }
    if let Some(t) = &s.target {
        out.push_str(&format!(" → {}", clip(&sym_label(t), 160)));
    }
    if s.confidence == Confidence::Heuristic {
        out.push_str(" (heuristic)");
    }
    if s.test {
        out.push_str(" (test)");
    }
    out.push('\n');
    out
}

pub fn render(r: &Trace) -> String {
    let mut s = String::new();
    if r.variant {
        s.push_str(&format!(
            "no key is the query as written; the closest differs in case or separators \
             (lower confidence): {:?}\n",
            r.key
        ));
    }
    let members = if r.members.is_empty() {
        String::new()
    } else {
        let n = r.members.len();
        let word = if n == 1 { "member" } else { "members" };
        format!(" in {n} {word} ({})", r.members.join(", "))
    };
    s.push_str(&format!("key {:?}{members}:\n", r.key));
    let block = |s: &mut String, title: &str, v: &[TraceSite], more: usize| {
        if v.is_empty() {
            return;
        }
        s.push_str(&format!("{title} ({}):\n", v.len() + more));
        for x in v {
            s.push_str(&line_of(x, true));
        }
        if more > 0 {
            s.push_str(&format!("  +{more} more\n"));
        }
    };
    if r.defined.is_empty() {
        s.push_str("defined: no enum constant or string constant has it\n");
    }
    block(&mut s, "defined", &r.defined, r.defined_more);
    block(
        &mut s,
        "runs it (cases, registrations)",
        &r.runs,
        r.runs_more,
    );
    block(&mut s, "lists it (catalogs)", &r.lists, r.lists_more);
    if !r.missing.is_empty() {
        s.push_str("missing from (holds its siblings, not it; it may be left out on purpose):\n");
        for m in &r.missing {
            s.push_str(&format!(
                "  {} ({}:{}): holds {} of the {} other keys of {} ({})\n",
                clip(&m.container, 120),
                clip(&short(&m.file), 140),
                m.line,
                m.holds,
                m.siblings,
                clip(&m.siblings_of, 100),
                clip(&short(&m.siblings_file), 140)
            ));
        }
    }
    let total = r.uses.len() + r.uses_more;
    if total > 0 {
        let tests = if r.uses_tests > 0 {
            format!(", {} in tests", r.uses_tests)
        } else {
            String::new()
        };
        s.push_str(&format!(
            "used ({total} in {}{tests}):\n",
            crate::text::files(r.uses_files)
        ));
        for u in &r.uses {
            s.push_str(&line_of(u, true));
        }
        if r.uses_more > 0 {
            s.push_str(&format!("  +{} more\n", r.uses_more));
        }
    }
    if !r.comments.is_empty() {
        let total = r.comments.len() + r.comments_more;
        let v: Vec<String> = r.comments.iter().map(|f| short(f)).collect();
        s.push_str(&format!(
            "named in comments of {}: {}{}\n",
            crate::text::files(total),
            clip(&v.join(", "), 400),
            crate::budget::more(r.comments_more)
        ));
    }
    if !r.also.is_empty() {
        let v: Vec<String> = r.also.iter().map(|(k, n)| format!("{k:?} ({n})")).collect();
        s.push_str(&format!(
            "also written as (case or separators differ, lower confidence): {}\n",
            v.join(", ")
        ));
    }
    s
}

/// `t` with at most `limit` uses (the ones left out counted), and at most twice as many
/// sites of each other kind: uses go first when the answer is too long.
fn limited(t: &Trace, limit: usize) -> Trace {
    let mut out = t.clone();
    out.uses_more = out.uses.len().saturating_sub(limit);
    out.uses.truncate(limit);
    let cap = limit.max(4) * 2;
    out.defined_more = out.defined.len().saturating_sub(cap);
    out.runs_more = out.runs.len().saturating_sub(cap);
    out.lists_more = out.lists.len().saturating_sub(cap);
    out.defined.truncate(cap);
    out.runs.truncate(cap);
    out.lists.truncate(cap);
    out.comments_more = out.comments.len().saturating_sub(4);
    out.comments.truncate(4);
    out
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Whether a query is written like a key: with a separator (`READ-FILE`, `users:get`,
/// `READ_FILE`), or in upper case (`EXPORT`).
pub fn looks_like_key(query: &str) -> bool {
    let q = query.trim().trim_matches(|c| matches!(c, '"' | '\'' | '`'));
    q.contains(['-', '_', ':', '.', '/'])
        || (q.chars().any(|c| c.is_uppercase()) && !q.chars().any(|c| c.is_lowercase()))
}

/// Whether `query` names a key as written (or as its constant), not only once case and
/// separators are ignored.
pub fn is_exact(repo: &crate::Repo, query: &str) -> bool {
    key_of(&repo.symbols.keys, query).is_some_and(|(_, variant)| !variant)
}

/// [`trace`] within [`limits::TRACE`].
pub fn answer(repo: &crate::Repo, query: &str) -> Option<Answer<Trace>> {
    let full = trace(repo, query)?;
    Some(fit(
        limits::TRACE,
        &[40, 24, 14, 8, 4, 2, 0],
        |limit| limited(&full, limit),
        render,
    ))
}

/// One line for `find` when paths hold the query and it is a string key too.
pub fn hint(repo: &crate::Repo, query: &str) -> Option<String> {
    let t = &repo.symbols.keys;
    let (key, variant) = key_of(t, query)?;
    let idx = t.by_key.get(&key)?;
    let count = |r: KeyRole| idx.iter().filter(|&&i| t.sites[i].role == r).count();
    let what = if variant {
        "a string key differs from it only in case or separators"
    } else {
        "it is also a string key"
    };
    Some(format!(
        "{what}: {key:?} ({} defined, {} run, {} listed); find with kind \"key\" traces it",
        count(KeyRole::Define),
        count(KeyRole::Provide),
        count(KeyRole::List)
    ))
}

#[cfg(test)]
mod tests {
    use super::squash;

    #[test]
    fn squash_ignores_case_and_separators() {
        assert_eq!(squash("READ-FILE"), "readfile");
        assert_eq!(squash("read_file"), "readfile");
        assert_eq!(squash("readFile"), "readfile");
    }
}
