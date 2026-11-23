//! Slices: the calls from the functions of one file to those of another, followed past
//! the second one over its bridges down to where the data is stored. `path` adds them when
//! the file it reaches calls out over HTTP or IPC (a client), so "from this view to that
//! client" also answers "and then what runs, down to which collection".
//!
//! Symbol level, over the calls and bridges of the symbol layer:
//! 1. From every function of `from` (and its top-level code), a breadth-first walk over
//!    calls and bridges finds each function of `to` it reaches, with the shortest chain.
//!    A bridge into a handler written inline in a registering function goes on with the
//!    calls on its lines only (`register()` registers several handlers).
//! 2. Past each one: its HTTP or IPC bridges (or those of a function of its own file it
//!    calls), to the handler on the other side.
//! 3. From the handler, a walk over calls where a call to an interface method goes on to
//!    the Spring beans that implement it (those injected where the call is made), down to
//!    methods that read or write a collection or table ([`crate::spring`]). The data sink
//!    shown is the nearest one that writes (for a request that is not a `GET`; the nearest
//!    that reads for a `GET`); the others reached are listed by name. Chains that write
//!    come first, then other requests that are not a `GET`, then reads.
//!
//! Everything is bounded: walk depths, visited functions, chains and sinks listed.

use crate::spring::{dispatch, reaching};
use crate::symbols::SymbolTable;
use cartograph_core::{Bridge, BridgeKind, DataSink, SymbolCall, SymbolKind, split_symbol_id};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;

/// Deepest chain from `from` to `to`.
const REACH_DEPTH: usize = 10;
/// Most functions the walk to `to` visits.
const REACH_VISITS: usize = 20_000;
/// Deepest walk from a handler to its data.
const SINK_DEPTH: usize = 8;
/// Most functions the walk to the data visits.
const SINK_VISITS: usize = 5_000;
/// Most bridges followed past one function of `to`.
const BRIDGES_PER_TARGET: usize = 4;
/// Most other sinks named.
const OTHER_SINKS: usize = 6;

/// One step of a slice: a function, where it hands over to the next step, and how it was
/// reached.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Hop {
    pub file: String,
    /// The line where the next step is called (or the data written), else the function's.
    pub line: u32,
    /// `Class.method`, `function`.
    pub symbol: String,
    /// How this step was reached from the one before: `ipc expose window.api.save`,
    /// `http axios POST /api/orders`, `Spring bean of OrderService`; `None` for a call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// Where a slice ends: a collection or table.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SinkRow {
    /// `Mongo`, `JPA`.
    pub store: String,
    pub name: String,
    /// The entity class, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// `path:line` of the call.
    pub at: String,
    /// The method that makes the call writes (by its name: `save…`, `insert…`, `update…`).
    pub writes: bool,
}

/// The slice past `to` over one of its bridges.
#[derive(Debug, Clone, Serialize)]
pub struct Continuation {
    /// The bridge's key: `POST /api/orders`, `order:save`.
    pub key: String,
    pub hops: Vec<Hop>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink: Option<SinkRow>,
    /// Other collections or tables the handler reaches (`counters (Mongo)`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_sinks: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub other_sinks_more: usize,
}

/// The calls from `from` to one function of `to`, and what runs past it.
#[derive(Debug, Clone, Serialize)]
pub struct Chain {
    pub hops: Vec<Hop>,
    pub continues: Vec<Continuation>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Chain {
    /// Whether the slice reaches a collection or table.
    pub fn has_sink(&self) -> bool {
        self.continues.iter().any(|c| c.sink.is_some())
    }
}

/// Whether `to` calls out over a bridge a slice follows (HTTP, or IPC other than a push).
pub fn crosses(t: &SymbolTable, to: &BTreeSet<String>) -> bool {
    to.iter().any(|p| t.bridges.from_file(p).any(followed))
}

fn followed(b: &Bridge) -> bool {
    match b.kind {
        BridgeKind::Http => b.dst.is_some(),
        BridgeKind::Ipc => b.subtype != "push" && b.dst.is_some(),
        BridgeKind::Package => false,
    }
}

/// An edge of the walk: to a node, the line where it leaves, how.
#[derive(Debug, Clone)]
struct Edge {
    to: Node,
    line: u32,
    via: Option<String>,
}

/// A node of the walk: a symbol (or `file:` for top-level code), maybe only the lines of
/// an inline handler in it (`[from, to)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Node {
    id: String,
    lines: Option<(u32, u32)>,
}

impl Node {
    fn whole(id: &str) -> Node {
        Node {
            id: id.to_string(),
            lines: None,
        }
    }
}

/// The call graph of the symbol layer, with bridges, built once per question.
struct Graph<'t> {
    t: &'t SymbolTable,
    /// Caller (`sym:…` or `file:…`) → its calls, by line.
    out: HashMap<&'t str, Vec<&'t SymbolCall>>,
    /// Source symbol → bridges.
    bridges_out: HashMap<&'t str, Vec<&'t Bridge>>,
    /// Destination symbol → the lines bridges land on it.
    landings: HashMap<&'t str, Vec<u32>>,
    /// Interface method → the bean methods Spring may run for it (see [`dispatch`]).
    impls: RefCell<HashMap<String, Rc<Impls<'t>>>>,
}

type Impls<'t> = Vec<crate::spring::Runs<'t>>;

impl<'t> Graph<'t> {
    fn build(t: &'t SymbolTable) -> Graph<'t> {
        let mut out: HashMap<&str, Vec<&SymbolCall>> = HashMap::new();
        for c in &t.calls {
            let caller = match &c.caller {
                Some(x) => x.as_str(),
                None => continue,
            };
            out.entry(caller).or_default().push(c);
        }
        for v in out.values_mut() {
            v.sort_by(|a, b| (a.line, &a.target).cmp(&(b.line, &b.target)));
        }
        let mut bridges_out: HashMap<&str, Vec<&Bridge>> = HashMap::new();
        let mut landings: HashMap<&str, Vec<u32>> = HashMap::new();
        for b in &t.bridges.bridges {
            if let Some(s) = &b.src {
                bridges_out.entry(s.as_str()).or_default().push(b);
            }
            if let Some(d) = &b.dst {
                landings.entry(d.as_str()).or_default().push(b.dst_line);
            }
        }
        for v in landings.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        Graph {
            t,
            out,
            bridges_out,
            landings,
            impls: RefCell::new(HashMap::new()),
        }
    }

    /// The node a bridge lands on: the handler, or the lines of an inline handler in the
    /// function that registers it (up to the next handler it registers).
    fn landing(&self, b: &Bridge) -> Option<Node> {
        let d = b.dst.as_deref()?;
        let sym = self.t.symbol(d)?;
        // A handler passed by reference is registered elsewhere: the whole function.
        let inline = b.dst_file == sym.file && sym.line < b.dst_line && b.dst_line <= sym.end_line;
        if !inline {
            return Some(Node::whole(d));
        }
        let next = self
            .landings
            .get(d)
            .and_then(|v| v.iter().find(|&&l| l > b.dst_line).copied())
            .unwrap_or(sym.end_line + 1);
        Some(Node {
            id: d.to_string(),
            lines: Some((b.dst_line, next)),
        })
    }

    /// Calls and bridges leaving a node (for top-level code, the file's calls outside any
    /// function).
    fn edges(&self, n: &Node, spring: bool) -> Vec<Edge> {
        let inside = |line: u32| n.lines.is_none_or(|(a, b)| line >= a && line < b);
        let mut out = Vec::new();
        for c in self.out.get(n.id.as_str()).into_iter().flatten() {
            if !inside(c.line) {
                continue;
            }
            if spring {
                // An interface or base class method: the beans Spring gives the caller.
                let impls = self.dispatch(&c.target);
                if !impls.is_empty() {
                    let kept = self.bean_edges(&impls, n, c);
                    if !kept.is_empty() {
                        out.extend(kept);
                        continue;
                    }
                }
            }
            out.push(Edge {
                to: Node::whole(&c.target),
                line: c.line,
                via: None,
            });
        }
        for b in self.bridges_out.get(n.id.as_str()).into_iter().flatten() {
            if !followed(b) || !inside(b.src_line) {
                continue;
            }
            if let Some(to) = self.landing(b) {
                out.push(Edge {
                    to,
                    line: b.src_line,
                    via: Some(crate::bridges::step_label(b)),
                });
            }
        }
        out
    }

    /// The edges of a call to a method `impls` run for, through Spring; empty for a call
    /// that runs the method it names: a base class's, from a class below it
    /// (`super.save()`, an inherited `save()`), or one no bean serves. An implementation
    /// that calls its interface calls another one (a decorator).
    fn bean_edges(&self, impls: &Impls<'t>, n: &Node, c: &SymbolCall) -> Vec<Edge> {
        let ty = crate::spring::type_of_method(self.t, &c.target);
        let picked = ty
            .as_deref()
            .and_then(|ty| reaching(self.t, Some(&n.id), &c.file, c.line, ty));
        if picked.is_none()
            && let (Some(ty), Some(own)) =
                (ty.as_deref(), crate::spring::type_of_method(self.t, &n.id))
            && !self
                .t
                .symbol(ty)
                .is_some_and(|s| s.kind == SymbolKind::Interface)
            && crate::spring::is_subtype(self.t, &own, ty)
        {
            return Vec::new();
        }
        let label = ty
            .as_deref()
            .map(|x| format!("Spring bean of {}", crate::spring::leaf(x)));
        impls
            .iter()
            .filter(|(_, beans)| {
                picked
                    .as_ref()
                    .is_none_or(|p| beans.iter().any(|b| p.iter().any(|x| std::ptr::eq(*x, *b))))
            })
            .map(|(m, _)| Edge {
                to: Node::whole(m),
                line: c.line,
                via: label.clone(),
            })
            .collect()
    }

    /// [`dispatch`], once per method.
    fn dispatch(&self, id: &str) -> Rc<Impls<'t>> {
        if let Some(v) = self.impls.borrow().get(id) {
            return Rc::clone(v);
        }
        let v = Rc::new(dispatch(self.t, id));
        self.impls
            .borrow_mut()
            .insert(id.to_string(), Rc::clone(&v));
        v
    }

    fn label(&self, id: &str) -> String {
        match self.t.symbol(id) {
            Some(s) => s.qualified(),
            None => split_symbol_id(id).map_or(id.to_string(), |(_, n)| n.to_string()),
        }
    }

    fn file_of(&self, id: &str) -> String {
        self.t
            .symbol(id)
            .map(|s| s.file.clone())
            .or_else(|| split_symbol_id(id).map(|(p, _)| p.to_string()))
            .unwrap_or_default()
    }

    fn line_of(&self, id: &str) -> u32 {
        self.t.symbol(id).map_or(0, |s| s.line)
    }
}

/// The path of a walk back from `end`, as hops: each function with the line where it
/// calls the next one.
fn hops_to(
    g: &Graph<'_>,
    parent: &HashMap<Node, (Node, Edge)>,
    end: &Node,
    last_line: Option<u32>,
) -> Vec<Hop> {
    let mut rev: Vec<(Node, Option<Edge>)> = vec![(end.clone(), None)];
    let mut cur = end.clone();
    while let Some((p, e)) = parent.get(&cur) {
        rev.push((p.clone(), Some(e.clone())));
        cur = p.clone();
        if rev.len() > REACH_DEPTH + SINK_DEPTH + 2 {
            break;
        }
    }
    rev.reverse();
    let mut hops = Vec::new();
    for i in 0..rev.len() {
        let (n, leave) = &rev[i];
        let arrived = if i == 0 { None } else { rev[i - 1].1.clone() };
        let line = match leave {
            Some(e) => e.line,
            None => last_line.unwrap_or_else(|| g.line_of(&n.id)),
        };
        hops.push(Hop {
            file: g.file_of(&n.id),
            line,
            symbol: g.label(&n.id),
            via: arrived.and_then(|e| e.via),
        });
    }
    hops
}

/// First words of a call or method that writes.
const WRITES: &[&str] = &[
    "insert",
    "save",
    "update",
    "upsert",
    "replace",
    "persist",
    "put",
    "create",
    "add",
    "delete",
    "remove",
    "write",
    "store",
    "merge",
    "bulk",
    "findAndModify",
    "findAndReplace",
    "findAndRemove",
    "findOneAnd",
];
/// First words of a call that reads.
const READS: &[&str] = &[
    "find",
    "get",
    "read",
    "query",
    "search",
    "count",
    "exists",
    "aggregate",
    "stream",
    "list",
    "load",
    "fetch",
];

/// `name` starts with one of `words`, as a word (`saveAll`, not `savedBy`).
fn starts(name: &str, words: &[&str]) -> bool {
    words.iter().any(|w| {
        name.strip_prefix(w)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_lowercase()))
    })
}

/// Whether a sink's call writes: the call itself says it (`save`, `findAndModify`,
/// `findById`); one that only opens the collection (`getCollection`) is what the method
/// around it is.
fn writes(d: &DataSink, method: &str) -> bool {
    let op = d.via.rsplit('.').next().unwrap_or(&d.via);
    if op != "getCollection" {
        if starts(op, WRITES) {
            return true;
        }
        if starts(op, READS) {
            return false;
        }
    }
    starts(method, WRITES)
}

/// A data sink the walk found: `((rank, depth, method's turn, line), method, sink, writes)`.
type Found<'t> = ((u8, usize, usize, u32), Node, &'t DataSink, bool);

/// From a handler: the nearest data sink (writes first unless `read`), and the other
/// collections reached.
fn to_data(g: &Graph<'_>, start: &Node, read: bool) -> (Vec<Hop>, Option<SinkRow>, Vec<String>) {
    let t = g.t;
    let mut parent: HashMap<Node, (Node, Edge)> = HashMap::new();
    let mut seen: HashSet<Node> = HashSet::from([start.clone()]);
    let mut queue: VecDeque<(Node, usize)> = VecDeque::from([(start.clone(), 0)]);
    let mut found: Vec<Found<'_>> = Vec::new();
    let mut visits = 0;
    while let Some((n, depth)) = queue.pop_front() {
        visits += 1;
        if visits > SINK_VISITS {
            break;
        }
        for d in t.spring.sinks_of(&n.id) {
            // A repository's or an entity's own row is not a call.
            if t.symbol(&n.id).is_some_and(|s| s.kind.is_type()) {
                continue;
            }
            let name = split_symbol_id(&n.id)
                .map(|(_, q)| q.rsplit('.').next().unwrap_or(q))
                .unwrap_or("");
            let w = writes(d, name);
            let rank = u8::from(w == read);
            found.push(((rank, depth, visits, d.line), n.clone(), d, w));
        }
        if depth >= SINK_DEPTH {
            continue;
        }
        for e in g.edges(&n, true) {
            if seen.insert(e.to.clone()) {
                parent.insert(e.to.clone(), (n.clone(), e.clone()));
                queue.push_back((e.to.clone(), depth + 1));
            }
        }
    }
    found.sort_by_key(|f| f.0);
    let Some((_, node, best, w)) = found.first() else {
        return (Vec::new(), None, Vec::new());
    };
    let hops = hops_to(g, &parent, node, Some(best.line));
    let sink = SinkRow {
        store: cartograph_core::spring::store_label(&best.store).to_string(),
        name: best.name.clone(),
        entity: best
            .entity
            .as_deref()
            .map(|e| crate::spring::leaf(e).to_string()),
        at: format!("{}:{}", best.file, best.line),
        writes: *w,
    };
    let mut others: Vec<String> = Vec::new();
    for (_, _, d, _) in &found {
        if d.name == best.name && d.store == best.store {
            continue;
        }
        let s = format!(
            "{} ({})",
            d.name,
            cartograph_core::spring::store_label(&d.store)
        );
        if !others.contains(&s) {
            others.push(s);
        }
    }
    (hops, Some(sink), others)
}

/// The chains from the functions of `from` to those of `to`, each continued past `to`
/// over its bridges (see the module docs). Those that reach data first, then those of
/// requests that change it, then by where they start.
pub fn chains(t: &SymbolTable, from: &BTreeSet<String>, to: &BTreeSet<String>) -> Vec<Chain> {
    let g = Graph::build(t);
    // Start: every function of `from`.
    let mut starts: Vec<Node> = Vec::new();
    for p in from {
        for &i in t.of_file(p) {
            let s = &t.symbols[i];
            if s.kind.is_callable() {
                starts.push(Node::whole(&s.id()));
            }
        }
    }
    let in_to = |id: &str| split_symbol_id(id).is_some_and(|(p, _)| to.contains(p));
    let mut parent: HashMap<Node, (Node, Edge)> = HashMap::new();
    let mut seen: HashSet<Node> = starts.iter().cloned().collect();
    let mut queue: VecDeque<(Node, usize)> = starts.iter().map(|n| (n.clone(), 0)).collect();
    let mut targets: Vec<Node> = Vec::new();
    let mut visits = 0;
    while let Some((n, depth)) = queue.pop_front() {
        visits += 1;
        if visits > REACH_VISITS {
            break;
        }
        if depth > 0 && in_to(&n.id) {
            // Stop at `to`: what runs past it is the continuation.
            targets.push(n);
            continue;
        }
        if depth >= REACH_DEPTH {
            continue;
        }
        for e in g.edges(&n, false) {
            // Only steps that leave `from`'s file once: the start is any of its functions.
            if starts.contains(&e.to) {
                continue;
            }
            if seen.insert(e.to.clone()) {
                parent.insert(e.to.clone(), (n.clone(), e.clone()));
                queue.push_back((e.to.clone(), depth + 1));
            }
        }
    }
    let mut out: Vec<(u8, u8, u32, Chain)> = Vec::new();
    for target in &targets {
        let hops = hops_to(&g, &parent, target, None);
        let start_line = hops.first().map_or(0, |h| h.line);
        // Its own bridges, else those of the functions of its file it calls.
        let mut over: Vec<&Bridge> = g
            .bridges_out
            .get(target.id.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|b| followed(b))
            .collect();
        if over.is_empty() {
            for e in g.edges(target, false) {
                if e.via.is_none() && in_to(&e.to.id) {
                    over.extend(
                        g.bridges_out
                            .get(e.to.id.as_str())
                            .into_iter()
                            .flatten()
                            .copied()
                            .filter(|b| followed(b)),
                    );
                }
            }
        }
        over.sort_by(|a, b| (a.src_line, &a.key).cmp(&(b.src_line, &b.key)));
        over.dedup_by(|a, b| a.key == b.key && a.dst == b.dst);
        let mut continues: Vec<(u32, Continuation)> = Vec::new();
        for b in over.into_iter().take(BRIDGES_PER_TARGET) {
            let Some(start) = g.landing(b) else { continue };
            let read = b.kind == BridgeKind::Http && b.key.starts_with("GET ");
            let (mut hops, sink, others) = to_data(&g, &start, read);
            if hops.is_empty() {
                hops.push(Hop {
                    file: g.file_of(&start.id),
                    line: b.dst_line,
                    symbol: g.label(&start.id),
                    via: None,
                });
            }
            if let Some(first) = hops.first_mut() {
                first.via = Some(crate::bridges::step_label(b));
            }
            continues.push((
                b.src_line,
                Continuation {
                    key: b.key.clone(),
                    hops,
                    sink,
                    other_sinks: others.iter().take(OTHER_SINKS).cloned().collect(),
                    other_sinks_more: others.len().saturating_sub(OTHER_SINKS),
                },
            ));
        }
        // Those that reach data first; writes before reads; a creation (`POST`) before an
        // update, a deletion, then the rest.
        continues.sort_by_key(|c| {
            let k = &c.1;
            let method = match k.key.split_whitespace().next().unwrap_or("") {
                "POST" => 0,
                "PUT" => 1,
                "PATCH" => 2,
                "DELETE" => 3,
                "GET" => 5,
                _ => 4,
            };
            (
                k.sink.is_none(),
                !k.sink.as_ref().is_some_and(|x| x.writes),
                method,
            )
        });
        let first_line = continues.first().map(|c| c.0);
        let continues: Vec<Continuation> = continues.into_iter().map(|c| c.1).collect();
        // The function of `to` hands over where it calls out.
        let mut hops = hops;
        if let (Some(last), Some(line)) = (hops.last_mut(), first_line) {
            last.line = line;
        }
        let chain = Chain { hops, continues };
        // Requests that write first, then the other changes, then reads.
        let kind = chain.continues.first().map_or(2, |c| {
            if c.sink.as_ref().is_some_and(|k| k.writes) {
                0
            } else if c.key.starts_with("GET ") {
                2
            } else {
                1
            }
        });
        out.push((u8::from(!chain.has_sink()), kind, start_line, chain));
    }
    out.sort_by_key(|a| (a.0, a.1, a.2));
    out.into_iter().map(|x| x.3).collect()
}
