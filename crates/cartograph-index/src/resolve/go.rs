//! Go import resolution.
//!
//! **Modules.** Every indexed `go.mod` is a module: its `module` path, `require`d module
//! paths and `replace` directives (block and single-line forms). A `go.work` adds nothing:
//! its modules are found through their own `go.mod`.
//!
//! **Target.** Go imports a package, and a package is a directory: every non-test `.go`
//! file of it is visible to the importer. An import of a package in the repo is therefore
//! one `Import` edge to the package's **Dir** node (`Resolved`), not one edge per file:
//! a package of 70 files imported from 120 files would otherwise make 8,400 file edges
//! and 70 import sites per statement. The Dir target flows through the Dir → Dir and
//! Package → Package aggregates like a Python namespace package. The one exception is a
//! package at the repo root, which has no Dir node: there the edge goes to each non-test
//! `.go` file of the root.
//!
//! **Order** for `import "p"` from file `f`:
//! 1. `""` → `not_found`. A relative path (`./x`, `../x`, pre-modules) → the dir next to
//!    `f`.
//! 2. `p` is a module path of the repo, or starts with one plus `/` (longest wins) → the
//!    dir under that module's root. Checked before the standard library because a module
//!    may be named without a dot (`module app`).
//! 3. A `replace` of `f`'s module whose old path prefixes `p`: a local target (`./x`,
//!    `../x`) → the dir there (`not_found` when it leaves the repo or is missing, plus a
//!    diagnostic on the go.mod); a module target → External `gomod:<new path>`.
//! 4. First path element without a dot → standard library, External `go:<p>`
//!    (`go:net/http`, also the cgo pseudo-package `go:C`), `Exact`.
//! 5. `f`'s module vendors its dependencies (`vendor/modules.txt`): the longest vendored
//!    module path prefixing `p` → External `gomod:<module>`, `Resolved`. Vendored code is
//!    third-party code; its files stay in the tree but imports of it are external.
//! 6. The longest `require`d module path prefixing `p` → External `gomod:<module>`,
//!    `Resolved`.
//! 7. Otherwise a guessed module path → External `gomod:<guess>`, `Heuristic`: three
//!    elements on `github.com` / `gitlab.com` / `bitbucket.org` / `golang.org`, two
//!    elsewhere, plus a following `/vN` major-version element.
//!
//! A package dir must hold at least one non-test `.go` file; a `_test.go` importer may
//! also import its own dir's test-only package. Otherwise `not_found`.

use super::{Index, Outcome, Target, join, parent};
use cartograph_core::{Confidence, Diagnostic};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
enum ReplaceTo {
    /// A local directory: the repo-relative dir when it is inside the repo, else the raw
    /// path written in go.mod.
    Local(Result<String, String>),
    Module(String),
}

#[derive(Debug, Clone)]
struct Module {
    /// Module path (`example.com/app`); empty when go.mod has no `module` line.
    path: String,
    /// Repo-relative dir of the go.mod.
    dir: String,
    requires: Vec<String>,
    replaces: Vec<(String, ReplaceTo)>,
    /// Module paths listed in `vendor/modules.txt`, if the module vendors.
    vendored: Option<Vec<String>>,
}

/// Immutable Go resolver (shared across threads).
pub struct Resolver<'i, 'a> {
    ix: &'i Index<'a>,
    /// Sorted by dir, deepest first (for "nearest go.mod").
    modules: Vec<Module>,
    /// Dir → (non-test `.go` files, `_test.go` files), sorted.
    packages: HashMap<String, (Vec<String>, Vec<String>)>,
}

fn is_test_file(path: &str) -> bool {
    path.ends_with("_test.go")
}

/// Whether `p` is `prefix` or starts with `prefix/`.
fn has_path_prefix(p: &str, prefix: &str) -> bool {
    !prefix.is_empty()
        && (p == prefix
            || (p.len() > prefix.len()
                && p.starts_with(prefix)
                && p.as_bytes()[prefix.len()] == b'/'))
}

/// `p` without `prefix` and its slash (`""` when equal).
fn rest_after<'p>(p: &'p str, prefix: &str) -> &'p str {
    p.get(prefix.len()..).unwrap_or("").trim_start_matches('/')
}

/// Drop a `//` comment and surrounding whitespace.
fn strip_comment(line: &str) -> &str {
    line.split_once("//").map_or(line, |(a, _)| a).trim()
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('`').to_string()
}

/// What go.mod says: `(module path, requires, replaces as (old, new raw))`.
type GoMod = (Option<String>, Vec<String>, Vec<(String, String)>);

fn parse_go_mod(text: &str) -> GoMod {
    let mut module = None;
    let mut requires = Vec::new();
    let mut replaces = Vec::new();
    let mut block: Option<&str> = None;
    for raw in text.lines() {
        let line = strip_comment(raw);
        if line.is_empty() {
            continue;
        }
        if let Some(b) = block {
            if line == ")" {
                block = None;
                continue;
            }
            match b {
                "require" => {
                    if let Some(m) = line.split_whitespace().next() {
                        requires.push(unquote(m));
                    }
                }
                "replace" => {
                    if let Some(r) = parse_replace(line) {
                        replaces.push(r);
                    }
                }
                _ => {}
            }
            continue;
        }
        let (directive, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        if rest == "(" {
            block = Some(match directive {
                "require" => "require",
                "replace" => "replace",
                _ => "other",
            });
            continue;
        }
        match directive {
            "module" => module = Some(unquote(rest)),
            "require" => {
                if let Some(m) = rest.split_whitespace().next() {
                    requires.push(unquote(m));
                }
            }
            "replace" => {
                if let Some(r) = parse_replace(rest) {
                    replaces.push(r);
                }
            }
            _ => {}
        }
    }
    (module, requires, replaces)
}

/// `old [v] => new [v]` → `(old, new)`.
fn parse_replace(line: &str) -> Option<(String, String)> {
    let (old, new) = line.split_once("=>")?;
    let old = unquote(old.split_whitespace().next()?);
    let new = unquote(new.split_whitespace().next()?);
    Some((old, new))
}

/// Module paths in `vendor/modules.txt` (`# path version` lines).
fn parse_modules_txt(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("# "))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// Guess the module path of a third-party import path (see the module docs, step 7).
fn guess_module(p: &str) -> String {
    let parts: Vec<&str> = p.split('/').collect();
    let take = match parts.first().copied() {
        Some("github.com" | "gitlab.com" | "bitbucket.org" | "golang.org") => 3,
        _ => 2,
    };
    let mut n = take.min(parts.len());
    if let Some(next) = parts.get(n)
        && next.len() > 1
        && next.starts_with('v')
        && next[1..].bytes().all(|b| b.is_ascii_digit())
    {
        n += 1;
    }
    parts[..n].join("/")
}

impl<'i, 'a> Resolver<'i, 'a> {
    pub fn new(ix: &'i Index<'a>) -> (Self, Vec<Diagnostic>) {
        let mut diagnostics = Vec::new();
        let mut packages: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
        let mut gomods: Vec<&str> = Vec::new();
        for f in &ix.files {
            let name = f.rsplit('/').next().unwrap_or(f);
            if name == "go.mod" {
                gomods.push(f);
            } else if name.ends_with(".go") {
                let e = packages.entry(parent(f).to_string()).or_default();
                if is_test_file(f) {
                    e.1.push(f.to_string());
                } else {
                    e.0.push(f.to_string());
                }
            }
        }
        gomods.sort_unstable();
        let mut modules = Vec::new();
        for gm in gomods {
            let dir = parent(gm).to_string();
            let Some(text) = ix.read_config(gm) else {
                diagnostics.push(Diagnostic {
                    path: gm.to_string(),
                    detail: "go.mod could not be read".into(),
                });
                continue;
            };
            let (path, requires, raw_replaces) = parse_go_mod(&text);
            if path.is_none() {
                diagnostics.push(Diagnostic {
                    path: gm.to_string(),
                    detail: "go.mod has no module directive".into(),
                });
            }
            let mut replaces = Vec::new();
            for (old, new) in raw_replaces {
                let local = new.starts_with("./")
                    || new.starts_with("../")
                    || new == "."
                    || new == ".."
                    || new.starts_with('/');
                let to = if local {
                    match ix
                        .join(&dir, &new)
                        .filter(|d| !new.starts_with('/') && ix.is_dir(d))
                    {
                        Some(d) => ReplaceTo::Local(Ok(d)),
                        None => {
                            diagnostics.push(Diagnostic {
                                path: gm.to_string(),
                                detail: format!(
                                    "replace {old} => {new}: the directory is not in the indexed repo"
                                ),
                            });
                            ReplaceTo::Local(Err(new))
                        }
                    }
                } else {
                    ReplaceTo::Module(new)
                };
                replaces.push((old, to));
            }
            let modules_txt = join(&dir, "vendor/modules.txt").unwrap_or_default();
            let vendored = ix
                .is_file(&modules_txt)
                .then(|| ix.read_config(&modules_txt).map(|t| parse_modules_txt(&t)))
                .flatten();
            modules.push(Module {
                path: path.unwrap_or_default(),
                dir,
                requires,
                replaces,
                vendored,
            });
        }
        modules.sort_by(|a, b| {
            super::depth(&b.dir)
                .cmp(&super::depth(&a.dir))
                .then(a.dir.cmp(&b.dir))
        });
        (
            Resolver {
                ix,
                modules,
                packages: packages.into_iter().collect(),
            },
            diagnostics,
        )
    }

    /// The module whose go.mod is nearest above `file`.
    fn module_of(&self, file: &str) -> Option<&Module> {
        self.modules
            .iter()
            .find(|m| super::is_ancestor(&m.dir, parent(file)))
    }

    /// The package in `dir`, as seen from `importer`.
    fn package(&self, importer: &str, dir: &str, spec: &str, why: &str) -> Vec<(String, Outcome)> {
        let one = |o| vec![(spec.to_string(), o)];
        let (code, tests) = match self.packages.get(dir) {
            Some(p) => (&p.0, &p.1),
            None => {
                let detail = if self.ix.is_dir(dir) {
                    format!("{why}: {} has no .go files", show(dir))
                } else {
                    format!("{why}: {} is not in the repo", show(dir))
                };
                return one(Outcome::not_found(detail));
            }
        };
        let own_test = is_test_file(importer) && parent(importer) == dir;
        if code.is_empty() && !(own_test && !tests.is_empty()) {
            return one(Outcome::not_found(format!(
                "{why}: {} has only _test.go files",
                show(dir)
            )));
        }
        if !self.ix.is_mount_root(dir) {
            return one(Outcome::Resolved {
                target: Target::Dir(dir.to_string()),
                confidence: Confidence::Resolved,
            });
        }
        // The repo root has no Dir node, and a mount's holds the whole mount: one edge per
        // file of the root package.
        let files = if code.is_empty() { tests } else { code };
        files
            .iter()
            .map(|f| {
                (
                    spec.to_string(),
                    Outcome::file(f.clone(), Confidence::Resolved),
                )
            })
            .collect()
    }

    /// Resolve one import path. Returns one `(specifier, outcome)`, or one per file for
    /// the repo-root package.
    pub fn resolve(&self, importer: &str, p: &str) -> Vec<(String, Outcome)> {
        let one = |o| vec![(p.to_string(), o)];
        if p.is_empty() {
            return one(Outcome::not_found("empty import path"));
        }
        if p.starts_with("./") || p.starts_with("../") || p == "." || p == ".." {
            return match join(parent(importer), p) {
                Some(dir) => self.package(importer, &dir, p, "relative import"),
                None => one(Outcome::not_found("relative import leaves the repo")),
            };
        }
        if let Some(m) = self
            .modules
            .iter()
            .filter(|m| has_path_prefix(p, &m.path))
            .max_by_key(|m| m.path.len())
        {
            let dir = join(&m.dir, rest_after(p, &m.path)).unwrap_or_default();
            return self.package(importer, &dir, p, &format!("module {}", m.path));
        }
        let own = self.module_of(importer);
        if let Some((old, to)) = own
            .into_iter()
            .flat_map(|m| m.replaces.iter())
            .filter(|(old, _)| has_path_prefix(p, old))
            .max_by_key(|(old, _)| old.len())
        {
            let rest = rest_after(p, old);
            return match to {
                ReplaceTo::Local(Ok(dir)) => {
                    let dir = join(dir, rest).unwrap_or_default();
                    self.package(importer, &dir, p, &format!("replace {old}"))
                }
                ReplaceTo::Local(Err(raw)) => one(Outcome::not_found(format!(
                    "replace {old} => {raw}: the directory is not in the indexed repo"
                ))),
                ReplaceTo::Module(new) => one(Outcome::external(
                    format!("gomod:{new}"),
                    Confidence::Resolved,
                )),
            };
        }
        let first = p.split('/').next().unwrap_or(p);
        if !first.contains('.') {
            return one(Outcome::external(format!("go:{p}"), Confidence::Exact));
        }
        let longest = |mods: &[String]| -> Option<String> {
            mods.iter()
                .filter(|m| has_path_prefix(p, m))
                .max_by_key(|m| m.len())
                .cloned()
        };
        if let Some(m) = own.and_then(|m| m.vendored.as_deref()).and_then(longest) {
            return one(Outcome::external(
                format!("gomod:{m}"),
                Confidence::Resolved,
            ));
        }
        if let Some(m) = own.and_then(|m| longest(&m.requires)) {
            return one(Outcome::external(
                format!("gomod:{m}"),
                Confidence::Resolved,
            ));
        }
        one(Outcome::external(
            format!("gomod:{}", guess_module(p)),
            Confidence::Heuristic,
        ))
    }
}

fn show(dir: &str) -> String {
    if dir.is_empty() {
        "the repo root".into()
    } else {
        format!("dir {dir}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_mod_directives() {
        let text = "module example.com/app // c\n\ngo 1.22\n\nrequire x.org/a v1.0.0\n\
            require (\n\tgithub.com/b/c/v2 v2.1.0 // indirect\n\t\"y.io/d\" v0.1.0\n)\n\
            replace x.org/a => ../a\nreplace (\n\ty.io/d v0.1.0 => z.io/d v0.2.0\n)\n";
        let (m, req, rep) = parse_go_mod(text);
        assert_eq!(m.as_deref(), Some("example.com/app"));
        assert_eq!(req, vec!["x.org/a", "github.com/b/c/v2", "y.io/d"]);
        assert_eq!(
            rep,
            vec![
                ("x.org/a".to_string(), "../a".to_string()),
                ("y.io/d".to_string(), "z.io/d".to_string())
            ]
        );
    }

    #[test]
    fn module_guesses() {
        assert_eq!(guess_module("github.com/a/b/c/d"), "github.com/a/b");
        assert_eq!(guess_module("github.com/a/b/v3/d"), "github.com/a/b/v3");
        assert_eq!(
            guess_module("golang.org/x/sync/errgroup"),
            "golang.org/x/sync"
        );
        assert_eq!(guess_module("go.uber.org/zap/zapcore"), "go.uber.org/zap");
        assert_eq!(guess_module("k8s.io/api"), "k8s.io/api");
        assert!(has_path_prefix("a/b/c", "a/b"));
        assert!(!has_path_prefix("a/bc", "a/b"));
        assert!(!has_path_prefix("a/b", ""));
        assert_eq!(rest_after("a/b/c", "a/b"), "c");
        assert_eq!(
            parse_modules_txt("# x.org/a v1.0.0\n## explicit\nx.org/a/p\n# y.io/b v0.1.0\n"),
            vec!["x.org/a", "y.io/b"]
        );
    }
}
