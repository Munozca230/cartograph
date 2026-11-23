//! Merge decoded SCIP indexes into the file graph and the symbol layer.
//!
//! Two passes, because of the order of an index run: [`Prepared::file_edges`] runs with
//! import resolution (before the derived folder and package edges), [`Prepared::merge_symbols`]
//! after N2 resolved calls.
//!
//! A document *covers* its file when its path maps to an indexed file that was not modified
//! after the SCIP file was written (a later edit moves lines, so its SCIP data would point
//! at the wrong places; N2 keeps such a file, counted as stale).

use super::{Loaded, NO_END, ScipData, ScipDoc};
use cartograph_core::{
    CallStats, Confidence, Edge, EdgeKind, Graph, NodeKind, ScipIndexStats, ScipStats, ScipSymbol,
    Symbol, SymbolCall, SymbolKind, Symbols, node_id,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// How a document's `relative_path` becomes a repo path: drop `strip/`, then put `add/`
/// in front.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Mapping {
    pub strip: String,
    pub add: String,
}

impl Mapping {
    pub fn apply(&self, doc_path: &str) -> Option<String> {
        let p = if self.strip.is_empty() {
            doc_path
        } else {
            doc_path.strip_prefix(&self.strip)?.strip_prefix('/')?
        };
        Some(if self.add.is_empty() {
            p.to_string()
        } else {
            format!("{}/{p}", self.add)
        })
    }

    /// The folder of the repo document paths are relative to, for messages (`""` = root).
    pub fn describe(&self) -> String {
        match (self.strip.is_empty(), self.add.is_empty()) {
            (true, _) => self.add.clone(),
            (false, true) => format!("-{}", self.strip),
            (false, false) => format!("-{}+{}", self.strip, self.add),
        }
    }
}

/// The local path of a `file://` URI (`file:///C:/x` → `C:/x`), percent-decoded.
fn uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = rest.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(h, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let s = String::from_utf8(out).ok()?;
    // `/C:/x` on Windows.
    let b = s.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[2] == b':' && b[1].is_ascii_alphabetic() {
        return Some(s[1..].to_string());
    }
    Some(s)
}

/// `path` relative to `base` with forward slashes, when inside it (both canonicalized).
fn inside(base: &Path, path: &Path) -> Option<String> {
    let base = std::fs::canonicalize(base).ok()?;
    let path = std::fs::canonicalize(path).ok()?;
    crate::walk::relative(&base, &path)
}

/// How `data`'s document paths map to repo paths. Candidates, best first on a tie: the
/// project root when it is a local folder inside the repo (or above it), the SCIP file's
/// folder, the repo root, then the trailing folders of the project root (for an index
/// written elsewhere, as in CI). The one under which most documents name an existing file
/// wins.
pub fn infer_mapping(
    data: &ScipData,
    root: &Path,
    scip_path: &Path,
    exists: &dyn Fn(&str) -> bool,
) -> Mapping {
    let mut cands: Vec<Mapping> = Vec::new();
    let project = uri_path(&data.project_root);
    if let Some(p) = &project {
        let p = Path::new(p);
        if let Some(add) = inside(root, p) {
            cands.push(Mapping {
                strip: String::new(),
                add,
            });
        }
        if let Some(strip) = inside(p, root).filter(|s| !s.is_empty()) {
            cands.push(Mapping {
                strip,
                add: String::new(),
            });
        }
    }
    if let Some(add) = scip_path.parent().and_then(|d| inside(root, d)) {
        cands.push(Mapping {
            strip: String::new(),
            add,
        });
    }
    cands.push(Mapping::default());
    if let Some(p) = &project {
        let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        for i in 1..parts.len() {
            cands.push(Mapping {
                strip: String::new(),
                add: parts[i..].join("/"),
            });
        }
    }
    let mut seen = HashSet::new();
    cands.retain(|c| seen.insert(c.clone()));
    let sample: Vec<&ScipDoc> = data.docs.iter().take(500).collect();
    let mut best = (0usize, Mapping::default());
    for c in cands {
        let score = sample
            .iter()
            .filter(|d| c.apply(&d.path).is_some_and(|p| exists(&p)))
            .count();
        if score > best.0 {
            best = (score, c);
        }
    }
    best.1
}

/// A document whose path is an indexed file.
struct PDoc<'a> {
    data: &'a ScipData,
    doc: &'a ScipDoc,
    path: String,
    /// The file changed after the SCIP file was written.
    stale: bool,
    /// The file on disk.
    disk: PathBuf,
}

/// SCIP indexes matched to the repo's files, ready for both passes.
pub struct Prepared<'a> {
    docs: Vec<PDoc<'a>>,
    /// Global symbol → the document (in `docs`) that defines or declares it.
    def_doc: HashMap<&'a str, usize>,
    /// Packages of the symbols the indexes define: the indexed project itself.
    own_packages: HashSet<(String, String)>,
    pub stats: ScipStats,
}

impl<'a> Prepared<'a> {
    /// Match the documents of `loaded` to the indexed `files` (paths of the graph). Each
    /// index describes the mount of its prefix: one of `mounts`, `(prefix, root on disk)`.
    pub fn new(
        loaded: &'a [Loaded],
        mounts: &[(&str, &Path)],
        files: &HashSet<&str>,
    ) -> Prepared<'a> {
        let mut p = Prepared {
            docs: Vec::new(),
            def_doc: HashMap::new(),
            own_packages: HashSet::new(),
            stats: ScipStats::default(),
        };
        let mut taken: HashSet<String> = HashSet::new();
        for l in loaded {
            let mut st = ScipIndexStats {
                path: l.label.clone(),
                cached: l.cached,
                ..ScipIndexStats::default()
            };
            let data = match &l.data {
                Ok(d) => d.as_ref(),
                Err(e) => {
                    st.error = Some(e.clone());
                    p.stats.indexes.push(st);
                    continue;
                }
            };
            let Some(&(prefix, root)) = mounts.iter().find(|(p, _)| *p == l.prefix) else {
                st.error = Some(format!("no folder `{}` is indexed", l.prefix));
                p.stats.indexes.push(st);
                continue;
            };
            let in_mount = |f: &str| files.contains(crate::paths::under(prefix, f).as_str());
            let map = infer_mapping(data, root, &l.path, &in_mount);
            st.tool = data.tool.clone();
            st.prefix = map.describe();
            st.documents = data.documents;
            st.occurrences = data.occurrences;
            st.unmatched = data.bad_paths;
            for doc in &data.docs {
                for d in &doc.defs {
                    if let Some(pkg) = package_key(&data.symbols[d.sym as usize])
                        && !p.own_packages.contains(&pkg)
                    {
                        p.own_packages.insert(pkg);
                    }
                }
                let rel = map.apply(&doc.path).filter(|f| in_mount(f));
                let Some(rel) = rel else {
                    st.unmatched += 1;
                    if st.unmatched_sample.len() < 5 {
                        st.unmatched_sample.push(doc.path.clone());
                    }
                    continue;
                };
                let path = crate::paths::under(prefix, &rel);
                if !taken.insert(path.clone()) {
                    // A second document for the same file (two indexes overlap): the first wins.
                    continue;
                }
                st.matched += 1;
                let disk = root.join(&rel);
                let stale = match (l.mtime, std::fs::metadata(&disk)) {
                    (Some(scip), Ok(m)) => m.modified().is_ok_and(|t| t > scip),
                    _ => false,
                };
                if stale {
                    st.stale += 1;
                }
                let di = p.docs.len();
                for s in doc
                    .defs
                    .iter()
                    .map(|d| d.sym)
                    .chain(doc.declared.iter().copied())
                {
                    p.def_doc.entry(&data.symbols[s as usize]).or_insert(di);
                }
                p.docs.push(PDoc {
                    data,
                    doc,
                    path,
                    stale,
                    disk,
                });
            }
            p.stats.indexes.push(st);
        }
        p
    }

    /// Whether any document matched a file (else both passes are no-ops).
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    fn covered(&self) -> impl Iterator<Item = &PDoc<'a>> {
        self.docs.iter().filter(|d| !d.stale)
    }

    /// File-level evidence (pass 1, before the derived edges): a covered file that
    /// references a symbol defined in another file gets a `uses` edge to it, and one that
    /// references a package's symbol gets an `import_external` edge to that library, both
    /// `exact` and only where no dependency (an import of the file or of its folder, or any
    /// other file dependency) exists yet. Files of one Go package are one unit: references
    /// between them add nothing.
    pub fn file_edges(&mut self, graph: &Graph) -> Vec<Edge> {
        let langs: HashMap<&str, Option<&str>> = graph
            .nodes_of(NodeKind::File)
            .map(|n| (n.path.as_str(), n.lang.as_deref()))
            .collect();
        let existing: HashSet<(&str, &str)> = graph
            .edges
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EdgeKind::Import
                        | EdgeKind::ImportExternal
                        | EdgeKind::Uses
                        | EdgeKind::Call
                        | EdgeKind::Extends
                        | EdgeKind::Implements
                )
            })
            .map(|e| (e.src.as_str(), e.dst.as_str()))
            .collect();
        let externals: HashSet<&str> = graph
            .nodes_of(NodeKind::External)
            .map(|n| n.id.as_str())
            .collect();
        // Bare library name (after the ecosystem) → ids, for the last-resort match.
        let mut by_bare: HashMap<&str, Vec<&str>> = HashMap::new();
        for id in &externals {
            if let Some((_, bare)) = id.strip_prefix("external:").and_then(|x| x.split_once(':')) {
                by_bare.entry(bare).or_default().push(id);
            }
        }
        let mut deps: BTreeSet<(String, String)> = BTreeSet::new();
        let mut libs: BTreeSet<(String, String)> = BTreeSet::new();
        let mut matched_pkgs: HashSet<(String, String)> = HashSet::new();
        let mut unmatched_pkgs: HashSet<(String, String)> = HashSet::new();
        let mut ext_memo: HashMap<&str, Option<String>> = HashMap::new();
        for d in self.covered() {
            let src_id = node_id(NodeKind::File, &d.path);
            let src_lang = langs.get(d.path.as_str()).copied().flatten();
            // Each target file or symbol is looked at once per source file.
            let mut seen_files: HashSet<usize> = HashSet::new();
            let mut seen_syms: HashSet<u32> = HashSet::new();
            let syms = d.doc.refs.iter().map(|r| r[0]);
            for sym in syms.chain(d.doc.imports.iter().map(|i| i[0])) {
                if !seen_syms.insert(sym) {
                    continue;
                }
                let s = d.data.symbols[sym as usize].as_str();
                if let Some(&di) = self.def_doc.get(s) {
                    let dst = &self.docs[di].path;
                    if *dst == d.path || !seen_files.insert(di) {
                        continue;
                    }
                    let same_go_package = src_lang == Some("go")
                        && langs.get(dst.as_str()).copied().flatten() == Some("go")
                        && parent(dst) == parent(&d.path);
                    let dst_id = node_id(NodeKind::File, dst);
                    let dir_id = match parent(dst) {
                        "" => node_id(NodeKind::Repo, ""),
                        p => node_id(NodeKind::Dir, p),
                    };
                    if same_go_package
                        || existing.contains(&(src_id.as_str(), dst_id.as_str()))
                        || existing.contains(&(src_id.as_str(), dir_id.as_str()))
                    {
                        continue;
                    }
                    deps.insert((src_id.clone(), dst_id));
                    continue;
                }
                let ext = ext_memo.entry(s).or_insert_with(|| {
                    let pkg = package_of(s)?;
                    let key = (pkg.manager.clone(), pkg.name.clone());
                    if self.own_packages.contains(&key) {
                        return None;
                    }
                    let hit = external_candidates(&pkg)
                        .into_iter()
                        .map(|c| format!("external:{c}"))
                        .find(|c| externals.contains(c.as_str()))
                        .or_else(|| match by_bare.get(pkg.name.as_str()).map(Vec::as_slice) {
                            Some([one]) => Some((*one).to_string()),
                            _ => None,
                        });
                    if hit.is_some() {
                        matched_pkgs.insert(key);
                    } else {
                        unmatched_pkgs.insert(key);
                    }
                    hit
                });
                if let Some(ext) = ext
                    && !existing.contains(&(src_id.as_str(), ext.as_str()))
                {
                    libs.insert((src_id.clone(), ext.clone()));
                }
            }
        }
        self.stats.deps_added = deps.len() as u64;
        self.stats.externals_added = libs.len() as u64;
        self.stats.packages_matched = matched_pkgs.len() as u64;
        self.stats.packages_unmatched = unmatched_pkgs.len() as u64;
        deps.into_iter()
            .map(|(a, b)| Edge::new(a, b, EdgeKind::Uses, Confidence::Exact))
            .chain(
                libs.into_iter()
                    .map(|(a, b)| Edge::new(a, b, EdgeKind::ImportExternal, Confidence::Exact)),
            )
            .collect()
    }

    /// Symbol layer (pass 2, after N2): match SCIP definitions to N2 symbols (same file and
    /// name, on the name's line or inside the definition's range), add the ones N2 missed
    /// when N2 models their kind, and make SCIP authoritative for the calls of covered files:
    /// an N2 call SCIP confirms (a reference on the same line to the same target) stays,
    /// exact; a reference inside a function or method to a function or method that N2 has
    /// no call for is an exact call from it when the text calls it (the name is followed by
    /// `(`, or by generic arguments and `(`), and a value reference (`items.map(f)`) is
    /// not a call; an unconfirmed
    /// N2 call of a covered file is dropped when SCIP knows its target, else kept as N2 found
    /// it. Covered files' call counts are rewritten to match.
    pub fn merge_symbols(&mut self, s: &mut Symbols) {
        let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, x) in s.symbols.iter().enumerate() {
            by_file.entry(x.file.clone()).or_default().push(i);
        }
        let mut sym_map: HashMap<&'a str, (String, SymbolKind)> = HashMap::new();
        let mut added: Vec<Symbol> = Vec::new();
        for d in &self.docs {
            let ours: &[usize] = by_file.get(&d.path).map_or(&[], Vec::as_slice);
            for def in &d.doc.defs {
                if def.name.is_empty() {
                    continue;
                }
                let key = d.data.symbols[def.sym as usize].as_str();
                let (line, end) = (def.line.saturating_add(1), def.end.saturating_add(1));
                let named = |i: &&usize| same_name(&s.symbols[**i].name, &def.name);
                if d.stale {
                    // Lines may have moved: match by name and container only, when unique.
                    let fits: Vec<usize> = ours
                        .iter()
                        .filter(named)
                        .filter(|&&i| {
                            def.container.is_none() || s.symbols[i].container == def.container
                        })
                        .copied()
                        .collect();
                    let ids: BTreeSet<String> = fits.iter().map(|&i| s.symbols[i].id()).collect();
                    if let (1, Some(&i)) = (ids.len(), fits.first()) {
                        sym_map.insert(key, (s.symbols[i].id(), s.symbols[i].kind));
                    }
                    continue;
                }
                let hit = ours
                    .iter()
                    .filter(named)
                    .find(|&&i| s.symbols[i].line == line)
                    .or_else(|| {
                        ours.iter()
                            .filter(named)
                            .find(|&&i| (line..=end).contains(&s.symbols[i].line))
                    });
                if let Some(&i) = hit {
                    self.stats.symbols_matched += 1;
                    sym_map.insert(key, (s.symbols[i].id(), s.symbols[i].kind));
                } else if let Some(kind) = def.kind {
                    let sym = Symbol {
                        file: d.path.clone(),
                        name: def.name.clone(),
                        container: def.container.clone(),
                        kind,
                        line,
                        end_line: end,
                        exported: false,
                    };
                    self.stats.symbols_added += 1;
                    sym_map.insert(key, (sym.id(), kind));
                    added.push(sym);
                }
            }
        }
        for x in &added {
            by_file
                .entry(x.file.clone())
                .or_default()
                .push(s.symbols.len());
            s.symbols.push(x.clone());
        }

        let covered: BTreeSet<String> = self.covered().map(|d| d.path.clone()).collect();
        // N2 calls of covered files, by (file, line, target).
        let mut n2: HashMap<(&str, u32, &str), Vec<Confidence>> = HashMap::new();
        for c in s.calls.iter().filter(|c| covered.contains(c.file.as_str())) {
            n2.entry((c.file.as_str(), c.line, c.target.as_str()))
                .or_default()
                .push(c.confidence);
        }
        let mut calls: Vec<SymbolCall> = Vec::new();
        // Per covered file: calls kept, and calls N2 did not have.
        let mut per_file: BTreeMap<String, u32> = BTreeMap::new();
        let mut new_per_file: BTreeMap<String, u32> = BTreeMap::new();
        let (mut refs, mut confirmed, mut upgraded, mut new_calls) = (0u64, 0u64, 0u64, 0u64);
        for d in self.covered() {
            let mut callables: Vec<(u32, u32, String)> = by_file
                .get(&d.path)
                .into_iter()
                .flatten()
                .map(|&i| &s.symbols[i])
                .filter(|x| x.kind.is_callable())
                .map(|x| (x.line, x.end_line.max(x.line), x.id()))
                .collect();
            callables.sort_by_key(|c| (c.0, std::cmp::Reverse(c.1)));
            // The file's text, read the first time a reference may be a call N2 missed.
            let mut source: Option<Option<Source>> = None;
            for &[sym, line0, end] in &d.doc.refs {
                let key = d.data.symbols[sym as usize].as_str();
                let Some((target, kind)) = sym_map.get(key) else {
                    continue;
                };
                refs += 1;
                let line = line0.saturating_add(1);
                let caller = caller_of(&callables, line);
                let before = n2
                    .get_mut(&(d.path.as_str(), line, target.as_str()))
                    .and_then(Vec::pop);
                match before {
                    Some(c) => {
                        confirmed += 1;
                        if c != Confidence::Exact {
                            upgraded += 1;
                        }
                    }
                    // Only a reference the text calls: a function passed or stored as a
                    // value is not a call.
                    None if kind.is_callable()
                        && caller.is_some()
                        && source
                            .get_or_insert_with(|| Source::read(&d.disk))
                            .as_ref()
                            .is_some_and(|t| t.called(line0, end, d.doc.encoding)) =>
                    {
                        new_calls += 1;
                        *new_per_file.entry(d.path.clone()).or_default() += 1;
                    }
                    None => continue,
                }
                *per_file.entry(d.path.clone()).or_default() += 1;
                calls.push(SymbolCall {
                    file: d.path.clone(),
                    line,
                    caller: caller.map(str::to_string),
                    target: target.clone(),
                    confidence: Confidence::Exact,
                });
            }
        }
        // An unconfirmed N2 call is dropped only when SCIP knows its target: SCIP would have
        // seen that call. A target SCIP does not define (a file outside the indexed project,
        // one no document matched, a stale one) keeps the call at its N2 confidence.
        let known: HashSet<&str> = sym_map.values().map(|(id, _)| id.as_str()).collect();
        let dropped: usize = n2
            .iter()
            .filter(|((_, _, t), _)| known.contains(t))
            .map(|(_, v)| v.len())
            .sum();
        s.calls
            .retain(|c| !covered.contains(c.file.as_str()) || !known.contains(c.target.as_str()));
        // N2 calls kept in covered files, per file and confidence.
        let mut kept: HashMap<(String, Confidence), u32> = HashMap::new();
        for c in s.calls.iter().filter(|c| covered.contains(c.file.as_str())) {
            *kept.entry((c.file.clone(), c.confidence)).or_default() += 1;
        }
        s.calls.extend(calls);
        // Covered files: every call to a symbol SCIP knows is exact. What N2 could not
        // resolve (libraries, builtins) is still counted as such, less the calls only SCIP
        // found (most of them are among those): an estimate, as SCIP does not tell calls
        // from other uses.
        let old: HashMap<String, CallStats> = s
            .stats
            .iter()
            .filter(|c| covered.contains(c.file.as_str()))
            .map(|c| (c.file.clone(), c.clone()))
            .collect();
        s.stats.retain(|c| !covered.contains(c.file.as_str()));
        for f in &covered {
            let kept = |c: Confidence| kept.get(&(f.clone(), c)).copied().unwrap_or(0);
            let st = CallStats {
                file: f.clone(),
                exact: per_file.get(f).copied().unwrap_or(0) + kept(Confidence::Exact),
                resolved: kept(Confidence::Resolved),
                heuristic: kept(Confidence::Heuristic),
                unresolved: old.get(f).map_or(0, |c| {
                    (c.unresolved + c.ambiguous)
                        .saturating_sub(new_per_file.get(f).copied().unwrap_or(0))
                }),
                ..CallStats::default()
            };
            if st.total() > 0 {
                s.stats.push(st);
            }
        }

        self.stats.files_covered = covered.len() as u64;
        self.stats.references = refs;
        self.stats.calls_confirmed = confirmed;
        self.stats.calls_upgraded = upgraded;
        self.stats.calls_added = new_calls;
        self.stats.calls_dropped = dropped as u64;
        s.scip.files = covered.into_iter().collect();
        s.scip.added = added
            .iter()
            .map(|x| ScipSymbol {
                id: x.id(),
                line: x.line,
            })
            .collect();
        s.scip.stats = Some(self.stats.clone());
        s.normalize();
    }
}

/// The innermost callable (sorted by start, longest first) whose lines hold `line`.
fn caller_of(callables: &[(u32, u32, String)], line: u32) -> Option<&str> {
    let i = callables.partition_point(|c| c.0 <= line);
    callables[..i]
        .iter()
        .rev()
        .find(|c| c.1 >= line)
        .map(|c| c.2.as_str())
}

/// A covered file's text, to tell a call from another use of a reference.
struct Source {
    text: String,
    /// Byte offset of each line's start.
    lines: Vec<usize>,
}

/// Most characters of generic arguments read between a name and its `(`.
const MAX_GENERIC: usize = 256;

impl Source {
    fn read(path: &Path) -> Option<Source> {
        let text = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
        let mut lines = vec![0];
        lines.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Some(Source { text, lines })
    }

    /// Whether the reference on 0-based `line` whose range ends at character `end` is a
    /// call: the text after it is `(`, or generic arguments then `(`. Character offsets
    /// count what `encoding` says; when it is unspecified, every encoding must agree.
    fn called(&self, line: u32, end: u32, encoding: u8) -> bool {
        let Some(&start) = self.lines.get(line as usize) else {
            return false;
        };
        let stop = self
            .lines
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.text.len());
        let text = &self.text[start..stop];
        let one = [encoding];
        let encodings: &[u8] = if (1..=3).contains(&encoding) {
            &one
        } else {
            &[1, 2, 3]
        };
        end != NO_END
            && encodings.iter().all(|&e| {
                byte_offset(text, end as usize, e)
                    .is_some_and(|o| opens_call(&self.text[start + o..]))
            })
    }
}

/// The byte offset in `line` of character offset `end` counted in `encoding` (1 UTF-8
/// bytes, 2 UTF-16 code units, 3 code points); `None` past the end or inside a character.
fn byte_offset(line: &str, end: usize, encoding: u8) -> Option<usize> {
    if encoding == 1 {
        return (end <= line.len() && line.is_char_boundary(end)).then_some(end);
    }
    let mut units = 0;
    for (i, ch) in line.char_indices() {
        if units >= end {
            return (units == end).then_some(i);
        }
        units += if encoding == 2 { ch.len_utf16() } else { 1 };
    }
    (units == end).then_some(line.len())
}

/// Whether `rest`, the text after a name, calls it: `(`, or generic arguments first
/// (`f<T>(`, Rust's `f::<T>(`), with any whitespace between.
fn opens_call(rest: &str) -> bool {
    let mut s = rest.trim_start();
    if let Some(t) = s
        .strip_prefix("::")
        .map(str::trim_start)
        .filter(|t| t.starts_with('<'))
    {
        s = t;
    }
    if s.starts_with('<') {
        let (mut depth, mut prev, mut close) = (0usize, ' ', None);
        for (i, ch) in s.char_indices().take(MAX_GENERIC) {
            match ch {
                '<' => depth += 1,
                // `=>` and `->` inside a function type close nothing.
                '>' if prev != '=' && prev != '-' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i + 1);
                        break;
                    }
                }
                ';' => return false,
                _ => {}
            }
            prev = ch;
        }
        let Some(i) = close else {
            return false;
        };
        s = s[i..].trim_start();
    }
    s.starts_with('(')
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Names equal up to SCIP's spelling of special methods (`<constructor>`, `<init>`).
fn same_name(ours: &str, scip: &str) -> bool {
    let trim = |s: &str| s.trim_start_matches('<').trim_end_matches('>').to_string();
    ours == scip || trim(ours) == trim(scip)
}

/// The package of a global symbol, with the names of its leading namespaces.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Package {
    manager: String,
    name: String,
    namespaces: Vec<String>,
}

/// `(manager, name)` of a global symbol from its leading fields (`scheme manager name
/// version …`), without parsing its descriptors; `.` stands for an empty field. A name
/// with an escaped space (`  `) is read as [`package_of`] would read it.
fn package_key(symbol: &str) -> Option<(String, String)> {
    if symbol.contains("  ") {
        return package_of(symbol).map(|p| (p.manager, p.name));
    }
    let mut it = symbol.splitn(4, ' ');
    let (_scheme, manager, name) = (it.next()?, it.next()?, it.next()?);
    let dot = |s: &str| {
        if s == "." {
            String::new()
        } else {
            s.to_string()
        }
    };
    let (manager, name) = (dot(manager), dot(name));
    (!(manager.is_empty() && name.is_empty())).then_some((manager, name))
}

fn package_of(symbol: &str) -> Option<Package> {
    use scip::types::descriptor::Suffix;
    let parsed = scip::symbol::parse_symbol(symbol).ok()?;
    let pkg = parsed.package.as_ref()?;
    if pkg.manager.is_empty() && pkg.name.is_empty() {
        return None;
    }
    let namespaces = parsed
        .descriptors
        .iter()
        .take_while(|d| {
            matches!(
                d.suffix.enum_value(),
                Ok(Suffix::Namespace | Suffix::Package)
            )
        })
        .map(|d| d.name.clone())
        .collect();
    Some(Package {
        manager: pkg.manager.clone(),
        name: pkg.name.clone(),
        namespaces,
    })
}

/// Library ids (without `external:`) a SCIP package may be in the graph, best first: the
/// ecosystems of the import resolvers (`npm:`, `node:`, `crates:`, `rust:`, `pypi:`,
/// `python:`, `maven:`, `jdk:`, `gomod:`, `go:`, `nuget:`, `dotnet:`).
fn external_candidates(p: &Package) -> Vec<String> {
    let name = p.name.as_str();
    let ns0 = p.namespaces.first().map(String::as_str).unwrap_or("");
    let mut v = Vec::new();
    match p.manager.as_str() {
        "npm" => {
            v.push(format!("npm:{name}"));
            if name == "@types/node" && !ns0.is_empty() {
                v.push(format!("node:{}", ns0.trim_start_matches("node:")));
            } else if let Some(t) = name.strip_prefix("@types/") {
                // `@types/scope__pkg` types `@scope/pkg`.
                match t.split_once("__") {
                    Some((scope, pkg)) => v.push(format!("npm:@{scope}/{pkg}")),
                    None => v.push(format!("npm:{t}")),
                }
            }
        }
        "cargo" => {
            for n in [
                name.to_string(),
                name.replace('_', "-"),
                name.replace('-', "_"),
            ] {
                v.push(format!("crates:{n}"));
                v.push(format!("rust:{n}"));
            }
        }
        "pip" | "python" | "pypi" => {
            if !ns0.is_empty() {
                let top = ns0.split('.').next().unwrap_or(ns0);
                v.push(format!("pypi:{top}"));
                v.push(format!("python:{top}"));
            }
            v.push(format!("pypi:{name}"));
            v.push(format!("pypi:{}", name.to_lowercase().replace('-', "_")));
        }
        "maven" => {
            if name == "jdk" || name.starts_with("jdk") {
                let pkg: Vec<&str> = p.namespaces.iter().map(String::as_str).take(2).collect();
                if !pkg.is_empty() {
                    v.push(format!("jdk:{}", pkg.join(".")));
                }
            }
            if let Some((group, _)) = name.split_once(':') {
                v.push(format!("maven:{group}"));
            }
            v.push(format!("maven:{name}"));
        }
        "gomod" | "go" => {
            if name == "std" || !name.contains('.') {
                let path: Vec<&str> = p.namespaces.iter().map(String::as_str).collect();
                if !path.is_empty() {
                    v.push(format!("go:{}", path.join("/")));
                }
            }
            v.push(format!("gomod:{name}"));
        }
        "nuget" | "dotnet" => {
            v.push(format!("nuget:{name}"));
            let two: Vec<&str> = name.split('.').take(2).collect();
            v.push(format!("dotnet:{}", two.join(".")));
            v.push(format!("nuget:{}", two.join(".")));
        }
        _ => {}
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_become_local_paths() {
        assert_eq!(
            uri_path("file:///home/me/my%20repo").as_deref(),
            Some("/home/me/my repo")
        );
        assert_eq!(
            uri_path("file:///C:/Users/me/repo").as_deref(),
            Some("C:/Users/me/repo")
        );
        assert_eq!(uri_path("https://x"), None);
    }

    #[test]
    fn mappings_strip_then_add() {
        let m = Mapping {
            strip: "repo".into(),
            add: String::new(),
        };
        assert_eq!(m.apply("repo/src/a.ts").as_deref(), Some("src/a.ts"));
        assert_eq!(m.apply("other/a.ts"), None);
        let m = Mapping {
            strip: String::new(),
            add: "web".into(),
        };
        assert_eq!(m.apply("src/a.ts").as_deref(), Some("web/src/a.ts"));
    }

    #[test]
    fn a_foreign_project_root_is_inferred_from_its_trailing_folders() {
        let data = ScipData {
            project_root: "file:///ci/build/checkout/packages/web".into(),
            docs: ["src/a.ts", "src/b.ts", "src/gone.ts"]
                .iter()
                .map(|p| ScipDoc {
                    path: (*p).into(),
                    ..ScipDoc::default()
                })
                .collect(),
            ..ScipData::default()
        };
        let files: HashSet<&str> = ["packages/web/src/a.ts", "packages/web/src/b.ts", "src/x.ts"]
            .into_iter()
            .collect();
        let tmp = std::env::temp_dir();
        let m = infer_mapping(&data, &tmp, &tmp.join("nowhere.scip"), &|f| {
            files.contains(f)
        });
        assert_eq!(m.add, "packages/web");
    }

    #[test]
    fn only_a_reference_followed_by_its_arguments_is_a_call() {
        for call in [
            "(x)",
            " (x)",
            "\n  (x)",
            "<T>(x)",
            "<Map<K, V[]>> (x)",
            "::<Vec<_>>()",
        ] {
            assert!(opens_call(call), "{call:?}");
        }
        assert!(opens_call("<(x: number) => void>(cb)"));
        for value in [", 10)", ")", ";", ".bind(this)", "", "<T; (x)"] {
            assert!(!opens_call(value), "{value:?}");
        }
        // `é` is 2 UTF-8 bytes, 1 UTF-16 unit; `😀` 4 bytes, 2 units, 1 code point.
        let line = "é😀f(x)\n";
        assert_eq!(byte_offset(line, 7, 1), Some(7));
        assert_eq!(byte_offset(line, 4, 2), Some(7));
        assert_eq!(byte_offset(line, 3, 3), Some(7));
        assert_eq!(byte_offset(line, 2, 2), None);
        assert_eq!(byte_offset(line, 99, 3), None);
        let src = Source {
            text: "items.map(normalize);\nnormalize(x);\n".into(),
            lines: vec![0, 22],
        };
        assert!(!src.called(0, 19, 0));
        assert!(src.called(1, 9, 0));
        assert!(!src.called(1, NO_END, 0));
        assert!(!src.called(5, 9, 0));
    }

    #[test]
    fn packages_map_to_the_resolvers_ecosystems() {
        let p = |m: &str, n: &str, ns: &[&str]| Package {
            manager: m.into(),
            name: n.into(),
            namespaces: ns.iter().map(|s| (*s).into()).collect(),
        };
        assert!(external_candidates(&p("npm", "react", &[])).contains(&"npm:react".into()));
        assert!(external_candidates(&p("npm", "@types/node", &["fs"])).contains(&"node:fs".into()));
        assert!(
            external_candidates(&p("cargo", "serde_json", &[]))
                .contains(&"crates:serde-json".into())
        );
        assert!(external_candidates(&p("cargo", "std", &[])).contains(&"rust:std".into()));
        assert!(
            external_candidates(&p("python", "requests", &["requests"]))
                .contains(&"pypi:requests".into())
        );
        assert!(
            external_candidates(&p("maven", "com.google.guava:guava", &[]))
                .contains(&"maven:com.google.guava".into())
        );
        assert!(
            external_candidates(&p("gomod", "github.com/x/y", &[]))
                .contains(&"gomod:github.com/x/y".into())
        );
    }
}
