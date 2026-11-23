//! String keys (see [`cartograph_core::key`]): every raw key site of a file
//! ([`RawKey`]) and every string constant ([`crate::symbols::Code::consts`]) read
//! through the constants the file sees (imports followed, Java enum constants included),
//! with what a case runs (the first call resolved in its body) and what a registration
//! binds the key to (its handler, resolved like a call).
//!
//! Precision first:
//! - a registration or a catalog entry counts only in a container that holds at least
//!   [`MIN_CONTAINER`] keys; one `new Foo("x", fn)` is a use;
//! - a use is kept only for a key that something defines, provides or lists, and that is
//!   not a plain lower-case word (`text`, `value`, `div`: every file has them);
//! - a key that is not read to one value (a hole, an unknown constant) is no key.

use super::bridge::{Consts, callables, member_of};
use super::{Bindings, Ctx, Hier, Out, caller_of, resolve_call};
use crate::symbols::strexpr::{Part, key_like};
use crate::symbols::{KeyForm, RustCallPaths};
use cartograph_core::{Confidence, Graph, KeyRole, KeySite, NodeKind, SymbolCall};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Fewest keys a registering or listing container needs for its sites to count as such.
pub const MIN_CONTAINER: usize = 3;

/// Constants sharing a prefix (`STEP_` of `STEP_OPEN`) in one class: at least this many
/// make a container of their own (`Steps.STEP_*`).
const MIN_PREFIX_GROUP: usize = 3;

/// The container of each string constant of a file, by its name: the class or object it
/// is in (`Steps`, `IPC`), narrowed to a shared name prefix (`Steps.STEP_*`) when enough
/// constants share it; module constants are `constants`.
fn const_containers(names: &[(&str, u32)]) -> Vec<(String, u32)> {
    let split = |n: &str| -> (String, String) {
        match n.rsplit_once('.') {
            Some((c, l)) => (c.to_string(), l.to_string()),
            None => (String::new(), n.to_string()),
        }
    };
    // Only constant names (`STEP_OPEN`): the members of an object (`end_if`, `end_for`)
    // are one map whatever their names.
    let prefix = |l: &str| -> Option<String> {
        if !l
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return None;
        }
        let i = l.find('_')?;
        (i > 0 && i + 1 < l.len()).then(|| l[..=i].to_string())
    };
    let mut groups: HashMap<(String, String), (usize, u32)> = HashMap::new();
    let mut firsts: HashMap<String, u32> = HashMap::new();
    for &(n, line) in names {
        let (c, l) = split(n);
        let f = firsts.entry(c.clone()).or_insert(line);
        *f = (*f).min(line);
        if let Some(p) = prefix(&l) {
            let e = groups.entry((c, p)).or_insert((0, line));
            e.0 += 1;
            e.1 = e.1.min(line);
        }
    }
    names
        .iter()
        .map(|&(n, _)| {
            let (c, l) = split(n);
            let base = if c.is_empty() {
                "constants".to_string()
            } else {
                c.clone()
            };
            if let Some(p) = prefix(&l)
                && let Some(&(count, line)) = groups.get(&(c.clone(), p.clone()))
                && count >= MIN_PREFIX_GROUP
            {
                let label = if c.is_empty() {
                    format!("{p}*")
                } else {
                    format!("{c}.{p}*")
                };
                return (label, line);
            }
            (base, firsts.get(&c).copied().unwrap_or(0))
        })
        .collect()
}

/// A name a program could have written as an identifier (`READ_FILE`, `open`).
fn is_identifier(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Resolved calls of each file, by line.
type CallsByFile<'s> = HashMap<&'s str, Vec<(u32, &'s str)>>;

/// Every key site of the repo.
pub(super) fn run(
    ctx: &Ctx<'_>,
    binds: &[Bindings<'_>],
    h: &Hier<'_>,
    rust_paths: &RustCallPaths,
    graph: &Graph,
    calls: &[SymbolCall],
) -> Vec<KeySite> {
    if ctx
        .files
        .iter()
        .all(|f| f.code.keys.is_empty() && f.code.consts.is_empty())
    {
        return Vec::new();
    }
    let members: HashSet<&str> = graph
        .nodes_of(NodeKind::Dir)
        .filter(|n| !n.path.contains('/') && n.meta.get("member").is_some())
        .map(|n| n.path.as_str())
        .collect();
    let consts = Consts::with_enums(ctx, binds);
    let mut by_file: CallsByFile<'_> = HashMap::new();
    for c in calls {
        by_file
            .entry(c.file.as_str())
            .or_default()
            .push((c.line, c.target.as_str()));
    }
    for v in by_file.values_mut() {
        v.sort();
    }
    // Enum constants by their last name, per file, for a bare `case READ_FILE:`.
    let enums_of: Vec<HashMap<&str, &str>> = ctx
        .files
        .iter()
        .map(|f| {
            f.code
                .keys
                .iter()
                .filter(|k| k.form == KeyForm::Enum)
                .filter_map(|k| {
                    let n = k.name.as_deref()?;
                    let last = n.rsplit('.').next()?;
                    match k.value.0.as_slice() {
                        [Part::Lit(v)] => Some((last, v.as_str())),
                        _ => None,
                    }
                })
                .collect()
        })
        .collect();
    let per_file: Vec<Vec<KeySite>> = (0..ctx.files.len())
        .into_par_iter()
        .map(|fi| {
            let f = &ctx.files[fi];
            let mut out: Vec<KeySite> = Vec::new();
            if f.code.keys.is_empty() && f.code.consts.is_empty() {
                return out;
            }
            let calls_here: &[(u32, &str)] = by_file.get(f.path).map_or(&[], |v| v.as_slice());
            let cs = callables(ctx, fi);
            let symbol_at =
                |line: u32| caller_of(&cs, line).map(|(_, s)| ctx.ids[s as usize].clone());
            // Constants define their keys.
            let names: Vec<(&str, u32)> = f
                .code
                .consts
                .iter()
                .map(|k| (k.name.as_str(), k.line))
                .collect();
            let containers = const_containers(&names);
            // A key of a map keyed by strings (`{"READ-FILE": "file"}`) is an entry there: its
            // value is no key.
            let entry_lines: HashSet<u32> = f
                .code
                .keys
                .iter()
                .filter(|k| k.form == KeyForm::Entry)
                .map(|k| k.line)
                .collect();
            for (k, (container, cline)) in f.code.consts.iter().zip(containers) {
                let last = k.name.rsplit('.').next().unwrap_or(&k.name);
                if entry_lines.contains(&k.line) || !is_identifier(last) {
                    continue;
                }
                let v = consts.eval(fi, &k.value, 0);
                if !v.known() || v.key.contains('*') || !key_like(&v.key) {
                    continue;
                }
                out.push(KeySite {
                    key: v.key,
                    role: KeyRole::Define,
                    form: "const".into(),
                    name: Some(k.name.clone()),
                    container,
                    container_file: f.path.to_string(),
                    container_line: cline,
                    file: f.path.to_string(),
                    line: k.line,
                    symbol: None,
                    target: None,
                    confidence: v.confidence,
                });
            }
            for k in &f.code.keys {
                // The key: a literal, a constant, or (a case of an enum switch) a bare name of
                // an enum constant the file sees.
                let (key, confidence) = {
                    let v = consts.eval(fi, &k.value, 0);
                    if v.known() && !v.key.contains('*') {
                        (v.key, v.confidence)
                    } else if let [Part::Ref(r)] = k.value.0.as_slice()
                        && k.form == KeyForm::Case
                        && !r.contains('.')
                    {
                        let b = &binds[fi];
                        let mut hits: Vec<&str> = b
                            .deps
                            .iter()
                            .chain(&b.scope)
                            .chain(std::iter::once(&fi))
                            .filter_map(|&t| enums_of[t].get(r.as_str()).copied())
                            .collect();
                        hits.sort_unstable();
                        hits.dedup();
                        match hits.as_slice() {
                            [one] => (one.to_string(), Confidence::Heuristic),
                            _ => continue,
                        }
                    } else {
                        continue;
                    }
                };
                if !key_like(&key) {
                    continue;
                }
                let (mut role, mut form) = match k.form {
                    KeyForm::Enum => (KeyRole::Define, "enum"),
                    KeyForm::Case => (KeyRole::Provide, "case"),
                    KeyForm::Register => (KeyRole::Provide, "register"),
                    KeyForm::Annotation => (KeyRole::Provide, "annotation"),
                    KeyForm::Entry => (KeyRole::List, "entry"),
                    KeyForm::Use => (KeyRole::Use, "use"),
                    KeyForm::Comment => (KeyRole::Use, "comment"),
                };
                let caller = caller_of(&cs, k.line).map(|(d, _)| d);
                // What it runs: a handler, else the first call resolved in its body.
                let mut target = k.handlers.iter().find_map(|c| {
                    match resolve_call(ctx, binds, h, rust_paths, fi, c, caller) {
                        Out::Hit(sid, _) => Some(ctx.ids[sid as usize].clone()),
                        _ => None,
                    }
                });
                if target.is_none()
                    && let Some((a, b)) = k.body
                {
                    let own = symbol_at(k.line);
                    target = calls_here
                        .iter()
                        .filter(|(l, _)| *l >= a && *l <= b)
                        .map(|(_, t)| *t)
                        .find(|t| own.as_deref() != Some(*t))
                        .map(str::to_string);
                }
                // A registration whose code is none of the repo's (`put("K", new Date())`)
                // binds the key to nothing found: an entry of a map.
                if k.form == KeyForm::Register && target.is_none() && k.body.is_none() {
                    (role, form) = (KeyRole::List, "entry");
                }
                let (container, container_file) = if k.form == KeyForm::Annotation {
                    // Spans files (an annotation): one per member.
                    let m = member_of(&members, f.path);
                    let label = if m.is_empty() {
                        k.container.clone()
                    } else {
                        format!("{} in {m}", k.container)
                    };
                    (label, String::new())
                } else {
                    (k.container.clone(), f.path.to_string())
                };
                out.push(KeySite {
                    key,
                    role,
                    form: form.into(),
                    name: k.name.clone(),
                    container,
                    container_file,
                    container_line: k.container_line,
                    file: f.path.to_string(),
                    line: k.line,
                    symbol: if k.form == KeyForm::Enum {
                        None
                    } else {
                        symbol_at(k.line)
                    },
                    target,
                    confidence,
                });
            }
            out
        })
        .collect();
    let mut out: Vec<KeySite> = per_file.into_iter().flatten().collect();
    // The literal of a definition or an entry is not a use of it as well.
    let placed: HashSet<(String, u32, String)> = out
        .iter()
        .filter(|s| s.role != KeyRole::Use)
        .map(|s| (s.file.clone(), s.line, s.key.clone()))
        .collect();
    out.retain(|s| {
        s.role != KeyRole::Use || !placed.contains(&(s.file.clone(), s.line, s.key.clone()))
    });
    // A registration or catalog entry with too few siblings is a use.
    let mut sizes: BTreeMap<(String, u32, String), HashSet<String>> = BTreeMap::new();
    for s in &out {
        if matches!(s.form.as_str(), "register" | "entry") {
            sizes
                .entry((
                    s.container_file.clone(),
                    s.container_line,
                    s.container.clone(),
                ))
                .or_default()
                .insert(s.key.clone());
        }
    }
    for s in &mut out {
        if matches!(s.form.as_str(), "register" | "entry")
            && sizes
                .get(&(
                    s.container_file.clone(),
                    s.container_line,
                    s.container.clone(),
                ))
                .is_some_and(|k| k.len() < MIN_CONTAINER)
        {
            s.role = KeyRole::Use;
            s.form = "use".to_string();
        }
    }
    // Uses only of keys something defines, provides or lists.
    let known: HashSet<String> = out
        .iter()
        .filter(|s| s.role != KeyRole::Use)
        .map(|s| s.key.clone())
        .collect();
    out.retain(|s| s.role != KeyRole::Use || (known.contains(&s.key) && !plain_word(&s.key)));
    out
}

/// A plain lower-case word (`text`, `value`): as a string, every file uses it.
fn plain_word(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::const_containers;

    #[test]
    fn constants_group_by_class_and_shared_prefix() {
        let got = const_containers(&[
            ("Steps.STEP_OPEN", 3),
            ("Steps.STEP_CLOSE", 4),
            ("Steps.STEP_SAVE", 5),
            ("Steps.STATE_OK", 7),
            ("Steps.LONE", 8),
            ("IPC.USERS_GET", 2),
            ("TOP_A", 1),
            ("MAP.end_if", 9),
            ("MAP.end_for", 10),
            ("MAP.end_while", 11),
        ]);
        let labels: Vec<&str> = got.iter().map(|g| g.0.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Steps.STEP_*",
                "Steps.STEP_*",
                "Steps.STEP_*",
                "Steps",
                "Steps",
                "IPC",
                "constants",
                "MAP",
                "MAP",
                "MAP"
            ]
        );
        assert_eq!(got[0].1, 3);
        assert_eq!(got[3].1, 3);
    }
}
