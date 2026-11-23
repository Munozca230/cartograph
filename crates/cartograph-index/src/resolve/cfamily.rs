//! C / C++ `#include` resolution.
//!
//! **With `compile_commands.json`** (at the repo root, else in `build/`; read from disk
//! even when gitignored): the include dirs of each entry (`-I`, `-iquote`, `-isystem`,
//! `-idirafter`, `/I`, joined or as the next argument; relative to the entry's
//! `directory`, which may itself be relative to the database's dir) are mapped into the
//! repo. A file with its own entry searches its own dirs; a file without one (a header)
//! searches the union of every entry's dirs, in order. Every hit is `Exact` (the dirs are
//! the build's own; for a header, which translation unit includes it is not checked).
//! A database larger than the size limit is still read (up to [`MAX_DB_BYTES`]); one that
//! cannot be read or parsed is a diagnostic, and resolution falls back to heuristics.
//! - `"x.h"`: the includer's dir, then `-iquote`, `-I`, system dirs.
//! - `<x>`: `-I`, then system dirs.
//!
//! **Without it** (heuristic):
//! - `"x.h"`: the includer's dir (`Heuristic` like the rest, although every compiler
//!   searches it first for the quote form);
//! - then, for both forms, dirs named `include` (`Heuristic`); several hits keep only the
//!   `include` dirs of the includer's own project (the parent of the `include` dir is an
//!   ancestor of the includer, deepest first); still several → `ambiguous` with the
//!   candidates;
//! - then files whose path ends with `/x` (`Heuristic`); several → `ambiguous`.
//!
//! **Not found in the repo**: a C standard header → External `c:<header>`, a C++ one →
//! `cpp:<header>` (`Exact`); any other angle include → External `system:<first segment>`
//! (`system:boost`, `system:sys`; `Resolved`); a quote include → `not_found` (generated at
//! build time, outside the repo, or missing).

use super::{Index, Outcome, depth, is_ancestor, join, parent};
use cartograph_core::{Confidence, Diagnostic};
use std::collections::HashMap;

/// Where `compile_commands.json` is looked for, in order.
const DATABASES: [&str; 2] = ["compile_commands.json", "build/compile_commands.json"];

/// Largest compilation database read (they often exceed the 1 MB file limit).
pub const MAX_DB_BYTES: u64 = 256 * 1024 * 1024;

const C_STD: &[&str] = &[
    "assert.h",
    "complex.h",
    "ctype.h",
    "errno.h",
    "fenv.h",
    "float.h",
    "inttypes.h",
    "iso646.h",
    "limits.h",
    "locale.h",
    "math.h",
    "setjmp.h",
    "signal.h",
    "stdalign.h",
    "stdarg.h",
    "stdatomic.h",
    "stdbit.h",
    "stdbool.h",
    "stdckdint.h",
    "stddef.h",
    "stdint.h",
    "stdio.h",
    "stdlib.h",
    "stdnoreturn.h",
    "string.h",
    "tgmath.h",
    "threads.h",
    "time.h",
    "uchar.h",
    "wchar.h",
    "wctype.h",
];

const CPP_STD: &[&str] = &[
    "algorithm",
    "any",
    "array",
    "atomic",
    "barrier",
    "bit",
    "bitset",
    "cassert",
    "cctype",
    "cerrno",
    "cfenv",
    "cfloat",
    "charconv",
    "chrono",
    "cinttypes",
    "climits",
    "clocale",
    "cmath",
    "codecvt",
    "compare",
    "complex",
    "concepts",
    "condition_variable",
    "coroutine",
    "csetjmp",
    "csignal",
    "cstdarg",
    "cstddef",
    "cstdint",
    "cstdio",
    "cstdlib",
    "cstring",
    "ctime",
    "cuchar",
    "cwchar",
    "cwctype",
    "deque",
    "exception",
    "execution",
    "expected",
    "filesystem",
    "flat_map",
    "flat_set",
    "format",
    "forward_list",
    "fstream",
    "functional",
    "future",
    "generator",
    "initializer_list",
    "iomanip",
    "ios",
    "iosfwd",
    "iostream",
    "istream",
    "iterator",
    "latch",
    "limits",
    "list",
    "locale",
    "map",
    "mdspan",
    "memory",
    "memory_resource",
    "mutex",
    "new",
    "numbers",
    "numeric",
    "optional",
    "ostream",
    "print",
    "queue",
    "random",
    "ranges",
    "ratio",
    "regex",
    "scoped_allocator",
    "semaphore",
    "set",
    "shared_mutex",
    "source_location",
    "span",
    "spanstream",
    "sstream",
    "stack",
    "stacktrace",
    "stdexcept",
    "stdfloat",
    "stop_token",
    "streambuf",
    "string",
    "string_view",
    "syncstream",
    "system_error",
    "thread",
    "tuple",
    "type_traits",
    "typeindex",
    "typeinfo",
    "unordered_map",
    "unordered_set",
    "utility",
    "valarray",
    "variant",
    "vector",
    "version",
];

/// Include search dirs (repo-relative; `""` is the repo root).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct SearchDirs {
    quote: Vec<String>,
    angle: Vec<String>,
    system: Vec<String>,
}

impl SearchDirs {
    fn merge(&mut self, other: &SearchDirs) {
        for (mine, theirs) in [
            (&mut self.quote, &other.quote),
            (&mut self.angle, &other.angle),
            (&mut self.system, &other.system),
        ] {
            for d in theirs {
                if !mine.contains(d) {
                    mine.push(d.clone());
                }
            }
        }
    }

    fn count(&self) -> usize {
        self.quote.len() + self.angle.len() + self.system.len()
    }
}

#[derive(Debug, Default)]
struct CompileDb {
    /// Repo-relative path of the database.
    path: String,
    /// Search dirs of every file with an entry.
    entries: HashMap<String, SearchDirs>,
    union: SearchDirs,
}

/// Immutable C/C++ resolver (shared across threads).
pub struct Resolver<'i, 'a> {
    ix: &'i Index<'a>,
    /// The compilation database of each mount that has one, by mount prefix.
    dbs: HashMap<String, CompileDb>,
    /// Every repo dir named `include`, sorted.
    include_dirs: Vec<&'a str>,
    /// Indexed files by file name.
    by_name: HashMap<&'a str, Vec<&'a str>>,
}

impl<'i, 'a> Resolver<'i, 'a> {
    pub fn new(ix: &'i Index<'a>) -> (Self, Vec<Diagnostic>) {
        let mut diagnostics = Vec::new();
        // One database per mount, looked for at the mount's root.
        let mut dbs = HashMap::new();
        for (prefix, _) in &ix.mounts {
            for path in DATABASES.map(|d| crate::paths::under(prefix, d)) {
                let path = path.as_str();
                let text = match ix.read_config(path) {
                    Some(t) => t,
                    None => match read_large(ix, path) {
                        Ok(Some(t)) => t,
                        Ok(None) => continue,
                        Err(e) => {
                            diagnostics.push(Diagnostic {
                                path: path.to_string(),
                                detail: format!("{e}; includes are resolved heuristically"),
                            });
                            continue;
                        }
                    },
                };
                match parse_db(ix, path, &text) {
                    Ok(d) => {
                        dbs.insert(prefix.to_string(), d);
                        break;
                    }
                    Err(e) => diagnostics.push(Diagnostic {
                        path: path.to_string(),
                        detail: format!("{e}; includes are resolved heuristically"),
                    }),
                }
            }
        }
        let mut include_dirs: Vec<&str> = ix
            .dirs
            .iter()
            .copied()
            .filter(|d| d.rsplit('/').next() == Some("include"))
            .collect();
        include_dirs.sort_unstable();
        let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
        for f in &ix.files {
            by_name
                .entry(f.rsplit('/').next().unwrap_or(f))
                .or_default()
                .push(f);
        }
        for v in by_name.values_mut() {
            v.sort_unstable();
        }
        (
            Resolver {
                ix,
                dbs,
                include_dirs,
                by_name,
            },
            diagnostics,
        )
    }

    /// Resolve `#include "path"` (`system` false) or `#include <path>` in `importer`.
    pub fn resolve(&self, importer: &str, path: &str, system: bool) -> Outcome {
        let p = path.trim().replace('\\', "/");
        if p.is_empty() {
            return Outcome::not_found("empty include");
        }
        if p.starts_with('/') || p.as_bytes().get(1) == Some(&b':') {
            return Outcome::not_found("absolute include path");
        }
        let near = (!system)
            .then(|| self.ix.join(parent(importer), &p))
            .flatten();
        match self.dbs.get(self.ix.mount_root(importer)) {
            Some(db) => self.with_db(db, importer, &p, system, near.as_deref()),
            None => self.heuristic(importer, &p, system, near.as_deref()),
        }
    }

    fn with_db(
        &self,
        db: &CompileDb,
        importer: &str,
        p: &str,
        system: bool,
        near: Option<&str>,
    ) -> Outcome {
        let dirs = db.entries.get(importer).unwrap_or(&db.union);
        let confidence = Confidence::Exact;
        if let Some(n) = near.filter(|n| self.ix.is_file(n)) {
            return Outcome::file(n.to_string(), Confidence::Exact);
        }
        let quote: &[String] = if system { &[] } else { &dirs.quote };
        for d in quote.iter().chain(&dirs.angle).chain(&dirs.system) {
            if let Some(c) = join(d, p).filter(|c| self.ix.is_file(c)) {
                return Outcome::file(c, confidence);
            }
        }
        self.not_in_repo(
            p,
            system,
            near,
            &format!(
                "not next to the includer nor in the {} include dirs of {}",
                dirs.count(),
                db.path
            ),
        )
    }

    fn heuristic(&self, importer: &str, p: &str, system: bool, near: Option<&str>) -> Outcome {
        if let Some(n) = near.filter(|n| self.ix.is_file(n)) {
            return Outcome::file(n.to_string(), Confidence::Heuristic);
        }
        if system && let Some(ext) = std_header(p) {
            return Outcome::external(ext, Confidence::Exact);
        }
        let hits: Vec<(&str, String)> = self
            .include_dirs
            .iter()
            .filter_map(|d| join(d, p).filter(|c| self.ix.is_file(c)).map(|c| (*d, c)))
            .collect();
        let hits: Vec<String> = if hits.len() > 1 {
            // The nearest project of the includer: the deepest `include` dir whose parent
            // is an ancestor of the includer.
            let own: Vec<(&str, String)> = hits
                .into_iter()
                .filter(|(d, _)| is_ancestor(parent(d), importer))
                .collect();
            let deepest = own.iter().map(|(d, _)| depth(d)).max().unwrap_or(0);
            own.into_iter()
                .filter(|(d, _)| depth(d) == deepest)
                .map(|(_, c)| c)
                .collect()
        } else {
            hits.into_iter().map(|(_, c)| c).collect()
        };
        if let Some(o) = pick(hits, "include dirs") {
            return o;
        }
        let name = p.rsplit('/').next().unwrap_or(p);
        let suffix = format!("/{p}");
        let matches: Vec<String> = self
            .by_name
            .get(name)
            .map(|v| {
                v.iter()
                    .filter(|f| **f == p || f.ends_with(&suffix))
                    .map(|f| (*f).to_string())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(o) = pick(matches, "files of the repo") {
            return o;
        }
        self.not_in_repo(
            p,
            system,
            near,
            "no file of the repo matches (no compile_commands.json)",
        )
    }

    fn not_in_repo(&self, p: &str, system: bool, near: Option<&str>, detail: &str) -> Outcome {
        if let Some(ext) = std_header(p) {
            return Outcome::external(ext, Confidence::Exact);
        }
        if system {
            let first = p.split('/').next().unwrap_or(p);
            return Outcome::external(format!("system:{first}"), Confidence::Resolved);
        }
        match near.and_then(|n| self.ix.skip_reason(n).map(|r| (n, r))) {
            Some((n, r)) => Outcome::not_found(format!("{n} was not indexed ({})", r.as_str())),
            None => Outcome::not_found(detail),
        }
    }
}

/// A database file that [`Index::read_config`] skipped for its size: `Ok(None)` if it
/// does not exist.
fn read_large(ix: &Index<'_>, path: &str) -> Result<Option<String>, String> {
    let abs = ix.on_disk(path);
    let Ok(meta) = std::fs::metadata(&abs) else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Ok(None);
    }
    if meta.len() > MAX_DB_BYTES {
        return Err(format!(
            "{path} is larger than {} MB",
            MAX_DB_BYTES / (1024 * 1024)
        ));
    }
    std::fs::read(&abs)
        .map(|b| Some(String::from_utf8_lossy(&b).into_owned()))
        .map_err(|e| format!("{path} could not be read: {e}"))
}

/// One candidate → `Heuristic` file; several → `ambiguous`; none → `None`.
fn pick(mut hits: Vec<String>, place: &str) -> Option<Outcome> {
    match hits.len() {
        0 => None,
        1 => hits.pop().map(|h| Outcome::file(h, Confidence::Heuristic)),
        n => {
            let shown: Vec<&str> = hits.iter().take(3).map(String::as_str).collect();
            let more = if n > 3 { ", …" } else { "" };
            Some(Outcome::ambiguous(format!(
                "{n} candidates in {place}: {}{more}",
                shown.join(", ")
            )))
        }
    }
}

/// External id of a standard header, if `p` is one.
fn std_header(p: &str) -> Option<String> {
    if C_STD.contains(&p) {
        Some(format!("c:{p}"))
    } else if CPP_STD.contains(&p) {
        Some(format!("cpp:{p}"))
    } else {
        None
    }
}

/// Parse a compilation database into repo-relative search dirs.
fn parse_db(ix: &Index<'_>, path: &str, text: &str) -> Result<CompileDb, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("{path} is not valid JSON: {e}"))?;
    let entries = v
        .as_array()
        .ok_or_else(|| format!("{path} is not a JSON array"))?;
    // Absolute paths map to the graph through the mount roots, the database's own first.
    let (own, own_root) = ix.mount_of(path).unwrap_or(("", std::path::Path::new(".")));
    let mut roots: Vec<(String, &str)> =
        repo_roots(own_root).into_iter().map(|r| (r, own)).collect();
    for (prefix, root) in ix.mounts.iter().filter(|(p, _)| *p != own) {
        roots.extend(repo_roots(root).into_iter().map(|r| (r, *prefix)));
    }
    let rel_dir = crate::paths::strip_mount(own, parent(path)).unwrap_or_default();
    let db_dir = roots
        .first()
        .map(|(r, _)| normalize(&format!("{r}/{rel_dir}")))
        .unwrap_or_default();
    let mut db = CompileDb {
        path: path.to_string(),
        ..CompileDb::default()
    };
    for e in entries {
        let dir = e.get("directory").and_then(|d| d.as_str()).unwrap_or(".");
        let dir = if is_absolute(dir) {
            normalize(dir)
        } else {
            normalize(&format!("{db_dir}/{dir}"))
        };
        let args: Vec<String> = match (e.get("arguments"), e.get("command")) {
            (Some(serde_json::Value::Array(a)), _) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            (_, Some(serde_json::Value::String(c))) => split_command(c),
            _ => Vec::new(),
        };
        let mut dirs = SearchDirs::default();
        let mut add = |kind: usize, value: &str| {
            let abs = if is_absolute(value) {
                normalize(value)
            } else {
                normalize(&format!("{dir}/{value}"))
            };
            if let Some(rel) = to_repo(&abs, &roots).filter(|r| ix.is_dir(r)) {
                let list = match kind {
                    0 => &mut dirs.quote,
                    1 => &mut dirs.angle,
                    _ => &mut dirs.system,
                };
                if !list.contains(&rel) {
                    list.push(rel);
                }
            }
        };
        let flags: [(&str, usize); 8] = [
            ("-iquote", 0),
            ("-isystem", 2),
            ("-idirafter", 2),
            ("--include-directory=", 1),
            ("/external:I", 2),
            ("-external:I", 2),
            ("-I", 1),
            ("/I", 1),
        ];
        let mut i = 0;
        while let Some(a) = args.get(i) {
            i += 1;
            for (flag, kind) in flags {
                if let Some(rest) = a.strip_prefix(flag) {
                    if !rest.is_empty() {
                        add(kind, rest);
                    } else if let Some(next) = args.get(i) {
                        add(kind, next);
                        i += 1;
                    }
                    break;
                }
            }
        }
        if let Some(file) = e.get("file").and_then(|f| f.as_str()) {
            let abs = if is_absolute(file) {
                normalize(file)
            } else {
                normalize(&format!("{dir}/{file}"))
            };
            if let Some(rel) = to_repo(&abs, &roots) {
                db.entries.entry(rel).or_default().merge(&dirs);
            }
        }
        db.union.merge(&dirs);
    }
    Ok(db)
}

/// Absolute forms of the repo root to match database paths against: canonical (without
/// the Windows `\\?\` prefix) and as given.
fn repo_roots(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(c) = std::fs::canonicalize(root) {
        out.push(normalize(&c.to_string_lossy()));
    }
    if let Ok(a) = std::path::absolute(root) {
        let a = normalize(&a.to_string_lossy());
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with('\\')
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// Forward slashes, no `\\?\` prefix, `.` and `..` folded.
fn normalize(p: &str) -> String {
    let p = p.replace('\\', "/");
    let p = p.strip_prefix("//?/").unwrap_or(&p);
    let (prefix, rest) = if let Some(r) = p.strip_prefix('/') {
        ("/".to_string(), r)
    } else if p.as_bytes().get(1) == Some(&b':') {
        let (drive, r) = p.split_at(2);
        (format!("{drive}/"), r.trim_start_matches('/'))
    } else {
        (String::new(), p)
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("{prefix}{}", parts.join("/"))
}

/// Graph path of a normalized absolute path (case-insensitive on Windows): relative to the
/// first of `roots` (absolute mount roots with their prefixes) it lies under.
fn to_repo(abs: &str, roots: &[(String, &str)]) -> Option<String> {
    roots.iter().find_map(|(r, prefix)| {
        let r = r.trim_end_matches('/');
        let (a, rb) = (abs.as_bytes(), r.as_bytes());
        let head = a.get(..rb.len())?;
        let same = if cfg!(windows) {
            head.eq_ignore_ascii_case(rb)
        } else {
            head == rb
        };
        if !same {
            return None;
        }
        match a.get(rb.len()) {
            None => Some(crate::paths::under(prefix, "")),
            Some(b'/') => abs
                .get(rb.len() + 1..)
                .map(|rel| crate::paths::under(prefix, rel)),
            Some(_) => None,
        }
    })
}

/// Split a shell command line: whitespace separates words; `"…"` and `'…'` quote;
/// a backslash escapes a quote, a space or another backslash (Windows paths keep theirs).
fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' && matches!(chars.peek(), Some('"' | '\\')) => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c == '\\' && matches!(chars.peek(), Some('"' | '\'' | ' ' | '\\')) => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            None => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_like_a_shell() {
        assert_eq!(
            split_command(r#"cc -I include -I"my dir" -DX=\"y\" 'a b' C:\src\x.c"#),
            vec![
                "cc",
                "-I",
                "include",
                "-Imy dir",
                "-DX=\"y\"",
                "a b",
                r"C:\src\x.c"
            ]
        );
    }

    #[test]
    fn paths_normalize_and_map_into_the_repo() {
        assert_eq!(
            normalize(r"\\?\C:\Repo\build\..\include"),
            "C:/Repo/include"
        );
        assert_eq!(normalize("/repo/./a//b/../c"), "/repo/a/c");
        let roots = vec![("C:/Repo".to_string(), "")];
        assert_eq!(
            to_repo("C:/Repo/include", &roots).as_deref(),
            Some("include")
        );
        assert_eq!(to_repo("C:/Repo", &roots).as_deref(), Some(""));
        assert_eq!(to_repo("C:/Repository/x", &roots), None);
        assert_eq!(to_repo("D:/other", &roots), None);
        if cfg!(windows) {
            assert_eq!(to_repo("c:/repo/src", &roots).as_deref(), Some("src"));
        }
    }

    #[test]
    fn standard_headers() {
        assert_eq!(std_header("stdio.h").as_deref(), Some("c:stdio.h"));
        assert_eq!(std_header("vector").as_deref(), Some("cpp:vector"));
        assert_eq!(std_header("boost/asio.hpp"), None);
    }
}
