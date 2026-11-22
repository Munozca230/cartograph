//! `hierarchy(sym)`, within [`limits::HIERARCHY`]: the supertypes of a class, struct,
//! interface or trait (the chain, up to five hops; those outside the repo by name) and its
//! direct subtypes, each with where it is defined. Given a method, the hierarchy of its type,
//! with the supertypes that also define the method and the subtypes that override it.

use crate::budget::{Answer, clip, fit, limits};
use crate::text::short;
use crate::{QueryError, Repo};
use cartograph_core::{Confidence, SymbolKind, split_symbol_id};
use serde::Serialize;
use std::collections::HashSet;

/// Hops of supertypes shown (the resolver follows as many).
const MAX_DEPTH: usize = 5;

/// One supertype in the chain.
#[derive(Debug, Clone, Serialize)]
pub struct SuperRow {
    /// 1 for a direct supertype.
    pub depth: usize,
    /// As written in the subtype.
    pub name: String,
    /// The repo type it resolved to; `None` outside the repo.
    pub id: Option<String>,
    pub kind: Option<SymbolKind>,
    /// `path:line` of its definition.
    pub at: Option<String>,
    pub confidence: Option<Confidence>,
    /// It defines the method asked about.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub defines: bool,
}

/// One direct subtype.
#[derive(Debug, Clone, Serialize)]
pub struct SubRow {
    pub id: String,
    pub kind: Option<SymbolKind>,
    /// `path:line` of its definition (of the `impl` block for a Rust trait).
    pub at: String,
    /// Its own direct subtypes.
    pub subtypes: usize,
    /// It overrides the method asked about.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub overrides: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct HierarchyResult {
    /// The type (`sym:…`, or `file:…` for a GDScript script without `class_name`).
    pub symbol: String,
    pub kind: Option<SymbolKind>,
    pub defined_at: Option<String>,
    /// The method the question named, when it named one.
    pub method: Option<String>,
    pub supers: Vec<SuperRow>,
    pub supers_more: usize,
    /// Direct subtypes.
    pub subtypes: usize,
    /// Of those, the ones that override `method`.
    pub overriding: usize,
    pub subs: Vec<SubRow>,
    pub subs_more: usize,
}

/// The type a symbol stands for, and the method when it is one.
fn type_of(repo: &Repo, input: &str) -> Result<(String, Option<String>), QueryError> {
    let t = &repo.symbols;
    // A GDScript script without `class_name`: its file id.
    let raw = input.trim().replace('\\', "/");
    let path = raw.strip_prefix("file:").unwrap_or(&raw);
    let script = format!("file:{}", path.strip_prefix("./").unwrap_or(path));
    if t.indexed && t.in_hierarchy(&script) {
        return Ok((script, None));
    }
    let id = t.resolve(input)?;
    let Some(sym) = t.symbol(&id) else {
        return Err(QueryError::UnknownNode {
            input: input.to_string(),
            suggestions: Vec::new(),
        });
    };
    if sym.kind.is_type() {
        return Ok((id, None));
    }
    let not_a_type = || {
        QueryError::BadArgument(format!(
            "{} is a {}, not a class, struct, interface or trait, nor a method of one",
            clip(&id, 160),
            sym.kind.as_str()
        ))
    };
    if !sym.kind.is_callable() {
        return Err(not_a_type());
    }
    let method = Some(sym.name.clone());
    let Some(cont) = &sym.container else {
        // A GDScript function: its script is the class.
        let script = format!("file:{}", sym.file);
        if t.in_hierarchy(&script) {
            return Ok((script, method));
        }
        let class = t
            .of_file(&sym.file)
            .iter()
            .map(|&i| &t.symbols[i])
            .find(|x| {
                x.kind == SymbolKind::Class && x.container.is_none() && t.in_hierarchy(&x.id())
            });
        return match class {
            Some(c) if sym.file.ends_with(".gd") => Ok((c.id(), method)),
            _ => Err(not_a_type()),
        };
    };
    // The type in the method's file, else (a Rust `impl` elsewhere) the one type of that
    // name in the hierarchy.
    let in_file: Vec<String> = t
        .of_file(&sym.file)
        .iter()
        .map(|&i| &t.symbols[i])
        .filter(|x| x.kind.is_type() && &x.name == cont)
        .map(|x| x.id())
        .collect();
    let mut found: Vec<String> = in_file;
    if found.is_empty() {
        found = t
            .symbols
            .iter()
            .filter(|x| x.kind.is_type() && &x.name == cont && t.in_hierarchy(&x.id()))
            .map(|x| x.id())
            .collect();
    }
    found.sort();
    found.dedup();
    match found.len() {
        1 => Ok((found.remove(0), method)),
        0 => Err(not_a_type()),
        n => Err(QueryError::Ambiguous {
            input: input.to_string(),
            count: n,
            candidates: found.into_iter().take(5).collect(),
        }),
    }
}

/// Whether type `id` defines `method` itself.
fn defines(repo: &Repo, id: &str, method: &str) -> bool {
    let t = &repo.symbols;
    let want = match id.strip_prefix("file:") {
        // A script's functions are top-level.
        Some(path) => format!("sym:{path}#{method}"),
        None => match split_symbol_id(id) {
            Some((path, name)) => {
                // A GDScript `class_name` is its script.
                if path.ends_with(".gd") && !name.contains('.') {
                    let top = format!("sym:{path}#{method}");
                    if t.symbol(&top).is_some() {
                        return true;
                    }
                }
                let leaf = name.rsplit('.').next().unwrap_or(name);
                format!("sym:{path}#{leaf}.{method}")
            }
            None => return false,
        },
    };
    t.symbol(&want).is_some()
}

/// Where a type id is defined.
fn at_of(repo: &Repo, id: &str) -> (Option<SymbolKind>, Option<String>) {
    match repo.symbols.symbol(id) {
        Some(s) => (Some(s.kind), Some(format!("{}:{}", s.file, s.line))),
        None => (None, id.strip_prefix("file:").map(|p| format!("{p}:1"))),
    }
}

/// `hierarchy(sym)` (see the module docs).
pub fn hierarchy(repo: &Repo, sym: &str) -> Result<Answer<HierarchyResult>, QueryError> {
    let t = &repo.symbols;
    if !t.indexed {
        return Err(QueryError::BadArgument(
            "this index has no symbols; re-run `cartograph index`".into(),
        ));
    }
    let (id, method) = type_of(repo, sym)?;
    let (kind, defined_at) = at_of(repo, &id);

    // The chain, depth first in declared order (the lookup order, but for Python diamonds).
    let mut supers = Vec::new();
    let mut seen = HashSet::from([id.clone()]);
    chain(repo, &id, 0, method.as_deref(), &mut seen, &mut supers);

    let mut subs: Vec<SubRow> = Vec::new();
    let mut sub_seen = HashSet::new();
    for e in t.subs_of(&id) {
        if !sub_seen.insert(e.sub.clone()) {
            continue;
        }
        let (k, at) = at_of(repo, &e.sub);
        // A Rust trait: where `impl Trait for Type` is.
        let at = if kind == Some(SymbolKind::Trait) {
            format!("{}:{}", e.file, e.line)
        } else {
            at.unwrap_or_else(|| format!("{}:{}", e.file, e.line))
        };
        let mut sub_ids = HashSet::new();
        subs.push(SubRow {
            id: clip(&e.sub, 240),
            kind: k,
            at: clip(&at, 200),
            subtypes: t.subs_of(&e.sub).filter(|x| sub_ids.insert(&x.sub)).count(),
            overrides: method.as_deref().is_some_and(|m| {
                // Or where the subtype declares it (a Rust `impl Trait for Type` block).
                defines(repo, &e.sub, m)
                    || split_symbol_id(&e.sub).is_some_and(|(_, name)| {
                        let leaf = name.rsplit('.').next().unwrap_or(name);
                        t.symbol(&format!("sym:{}#{leaf}.{m}", e.file)).is_some()
                    })
            }),
        });
    }
    // Overriding subtypes first (for a method), then those with subtypes of their own.
    subs.sort_by(|a, b| {
        (b.overrides, b.subtypes)
            .cmp(&(a.overrides, a.subtypes))
            .then(a.id.cmp(&b.id))
    });
    let overriding = subs.iter().filter(|s| s.overrides).count();
    let steps: [(usize, usize); 8] = [
        (30, 40),
        (20, 25),
        (14, 16),
        (10, 10),
        (6, 6),
        (4, 4),
        (2, 2),
        (1, 1),
    ];
    let idx: Vec<usize> = (0..steps.len()).collect();
    Ok(fit(
        limits::HIERARCHY,
        &idx,
        |k| {
            let (max_supers, max_subs) = steps[k.min(steps.len() - 1)];
            HierarchyResult {
                symbol: clip(&id, 240),
                kind,
                defined_at: defined_at.as_ref().map(|a| clip(a, 200)),
                method: method.clone(),
                supers: supers.iter().take(max_supers).cloned().collect(),
                supers_more: supers.len().saturating_sub(max_supers),
                subtypes: subs.len(),
                overriding,
                subs: subs.iter().take(max_subs).cloned().collect(),
                subs_more: subs.len().saturating_sub(max_subs),
            }
        },
        render,
    ))
}

/// The supertypes of `cur`, each followed by its own, every type once.
fn chain(
    repo: &Repo,
    cur: &str,
    depth: usize,
    method: Option<&str>,
    seen: &mut HashSet<String>,
    out: &mut Vec<SuperRow>,
) {
    if depth == MAX_DEPTH {
        return;
    }
    for e in repo.symbols.supers_of(cur) {
        if let Some(x) = &e.target
            && !seen.insert(x.clone())
        {
            continue;
        }
        let (kind, at) = match &e.target {
            Some(x) => at_of(repo, x),
            None => (None, None),
        };
        out.push(SuperRow {
            depth: depth + 1,
            name: clip(&e.name, 80),
            id: e.target.as_ref().map(|x| clip(x, 240)),
            kind,
            at: at.map(|a| clip(&a, 200)),
            confidence: e.confidence,
            defines: match (method, &e.target) {
                (Some(m), Some(x)) => defines(repo, x, m),
                _ => false,
            },
        });
        if let Some(x) = &e.target {
            chain(repo, x, depth + 1, method, seen, out);
        }
    }
}

fn name_of(id: &str) -> String {
    match split_symbol_id(id) {
        Some((_, n)) => clip(n, 80),
        None => clip(&short(id), 120),
    }
}

fn render(r: &HierarchyResult) -> String {
    let mut s = format!("hierarchy of {}", clip(&r.symbol, 160));
    let mut what = Vec::new();
    if let Some(k) = r.kind {
        what.push(k.as_str().to_string());
    }
    if let Some(a) = &r.defined_at {
        what.push(clip(a, 160));
    }
    if !what.is_empty() {
        s.push_str(&format!(" ({})", what.join(", ")));
    }
    if let Some(m) = &r.method {
        s.push_str(&format!(" · method {m}"));
    }
    s.push('\n');
    if r.supers.is_empty() {
        s.push_str("supertypes: none\n");
    } else {
        s.push_str("supertypes (declared order, depth first):\n");
        for x in &r.supers {
            let indent = "  ".repeat(x.depth);
            match &x.id {
                Some(id) => {
                    let mut line = format!("{indent}{}", name_of(id));
                    if let Some(k) = x.kind {
                        line.push_str(&format!(" {}", k.as_str()));
                    }
                    if let Some(a) = &x.at {
                        line.push_str(&format!(" {}", clip(a, 140)));
                    }
                    if let Some(c) = x.confidence {
                        line.push_str(&format!(" [{c}]"));
                    }
                    if x.defines {
                        line.push_str(" · defines it");
                    }
                    s.push_str(&line);
                }
                None => s.push_str(&format!("{indent}{} (not in the repo)", x.name)),
            }
            s.push('\n');
        }
        if r.supers_more > 0 {
            s.push_str(&format!("  +{} more\n", r.supers_more));
        }
    }
    if r.subtypes == 0 {
        s.push_str("subtypes: none in the repo\n");
        return s;
    }
    let over = if r.method.is_some() {
        format!(", {} override it", r.overriding)
    } else {
        String::new()
    };
    s.push_str(&format!("subtypes ({}{over}):\n", r.subtypes));
    for x in &r.subs {
        let mut line = format!("  {}", name_of(&x.id));
        if let Some(k) = x.kind {
            line.push_str(&format!(" {}", k.as_str()));
        }
        line.push_str(&format!(" {}", clip(&x.at, 140)));
        if x.subtypes > 0 {
            line.push_str(&format!(" · {} subtypes", x.subtypes));
        }
        if x.overrides {
            line.push_str(" · overrides it");
        }
        s.push_str(&line);
        s.push('\n');
    }
    if r.subs_more > 0 {
        s.push_str(&format!("  +{} more\n", r.subs_more));
    }
    s
}
