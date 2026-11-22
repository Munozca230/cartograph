//! Python import resolution.
//!
//! **Source roots.** Each root has a dir (where top-level modules live) and a scope (the
//! files that may import from it):
//! - the repo root (scope: everything);
//! - every dir with `pyproject.toml`, `setup.py` or `setup.cfg` (a project), and its `src/`
//!   (scope: the project dir, so `tests/` next to `src/` sees the src-layout package);
//! - every other dir named `src` (scope: its parent);
//! - the parent of every top-level package, i.e. a dir with `__init__.py` whose parent has
//!   none (scope: that parent).
//!
//! **Absolute imports** (`import a.b`, `from a.b import c`), first hit wins:
//! 1. **Heuristic:** the importer's own dir when it is not a package (no `__init__.py`):
//!    a script run directly has its dir on `sys.path`. Whether it is run that way is a
//!    guess, so this is `Heuristic` unless that dir is also an applicable root.
//! 2. Applicable roots (scope contains the importer), nearest scope first → `Resolved`.
//!    `a/b.py`, `a/b.pyi`, `a/b/__init__.py`, `a/b/__init__.pyi`, else the dir `a/b` as a
//!    namespace package (an `Import` edge to the Dir node). A namespace dir counts only
//!    when it is nested in a regular package, or when the import lands on a file in it
//!    (`from plugins import extra` → `plugins/extra.py`): per PEP 420 an installed regular
//!    package wins over a bare folder, so `alembic/` or `docker/` folders do not shadow
//!    `pypi:alembic` / `pypi:docker`.
//! 3. If the top-level name is a local module or regular package (step 1 or 2) but the
//!    full path is missing: `not_found` (a local package, not a PyPI one).
//! 4. Standard library → External `python:<top>`, `Exact`.
//! 5. **Heuristic:** project roots whose scope does not contain the importer (another
//!    project of a monorepo, maybe installed in development mode): one distinct hit →
//!    `Heuristic`; several → `ambiguous`.
//! 6. Otherwise External `pypi:<top>` (the top-level module name, which may differ from
//!    the distribution name), `Resolved`.
//!
//! **Relative imports** walk up from the importer's dir (one dot = its own package) and
//! resolve the rest there → `Exact`. Leaving the repo is `not_found`.
//!
//! **Imported names.** For `from M import n`, if `M.n` is a module (`M/n.py` or
//! `M/n/__init__.py`) the edge goes to it, otherwise to `M` itself. One statement yields one
//! import site per distinct target.

use super::{Index, Outcome, Target, builtins, depth, is_ancestor, join, parent};
use cartograph_core::Confidence;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Root {
    dir: String,
    scope: String,
    project: bool,
}

/// Immutable Python resolver (shared across threads).
pub struct Resolver<'i, 'a> {
    ix: &'i Index<'a>,
    /// Sorted: deepest scope first, then deepest dir.
    roots: Vec<Root>,
    /// Dirs holding `__init__.py` / `__init__.pyi`.
    packages: BTreeSet<String>,
}

const PROJECT_FILES: [&str; 3] = ["pyproject.toml", "setup.py", "setup.cfg"];

impl<'i, 'a> Resolver<'i, 'a> {
    pub fn new(ix: &'i Index<'a>) -> Self {
        let mut packages = BTreeSet::new();
        let mut roots = BTreeSet::new();
        roots.insert(Root {
            dir: String::new(),
            scope: String::new(),
            project: false,
        });
        for f in &ix.files {
            let name = f.rsplit('/').next().unwrap_or(f);
            let dir = parent(f);
            if name == "__init__.py" || name == "__init__.pyi" {
                packages.insert(dir.to_string());
            }
            if PROJECT_FILES.contains(&name) {
                roots.insert(Root {
                    dir: dir.to_string(),
                    scope: dir.to_string(),
                    project: true,
                });
                if let Some(src) = join(dir, "src").filter(|s| ix.is_dir(s)) {
                    roots.insert(Root {
                        dir: src,
                        scope: dir.to_string(),
                        project: true,
                    });
                }
            }
        }
        for d in &ix.dirs {
            if d.rsplit('/').next() == Some("src") {
                roots.insert(Root {
                    dir: d.to_string(),
                    scope: parent(d).to_string(),
                    project: false,
                });
            }
        }
        for p in &packages {
            if !p.is_empty() && !packages.contains(parent(p)) {
                let top = parent(p).to_string();
                roots.insert(Root {
                    dir: top.clone(),
                    scope: top,
                    project: false,
                });
            }
        }
        let mut roots: Vec<Root> = roots.into_iter().collect();
        roots.sort_by(|a, b| {
            depth(&b.scope)
                .cmp(&depth(&a.scope))
                .then(depth(&b.dir).cmp(&depth(&a.dir)))
                .then(a.cmp(b))
        });
        // The same dir may be a root for several reasons; keep its most specific entry.
        let mut seen = BTreeSet::new();
        roots.retain(|r| seen.insert((r.dir.clone(), r.project)));
        Resolver {
            ix,
            roots,
            packages,
        }
    }

    /// Module `dotted` (no leading dots) under `dir`: a module file or a regular package,
    /// else a namespace package, but only one nested in a regular package (PEP 420: a
    /// regular package or module anywhere on `sys.path` wins over a bare directory, so a
    /// top-level folder such as `alembic/` or `docker/` is not taken for a package).
    fn module_in(&self, dir: &str, dotted: &str) -> Option<Target> {
        let base = join(dir, &dotted.replace('.', "/"))?;
        for c in [
            format!("{base}.py"),
            format!("{base}.pyi"),
            format!("{base}/__init__.py"),
            format!("{base}/__init__.pyi"),
        ] {
            if self.ix.is_file(&c) {
                return Some(Target::File(c));
            }
        }
        let nested = base != dir && self.packages.contains(parent(&base));
        (!base.is_empty() && nested && self.ix.is_dir(&base)).then_some(Target::Dir(base))
    }

    /// `module_in`, plus a top-level namespace package when the import lands on a file in
    /// it: `from plugins import extra` with `plugins/extra.py` and no `plugins/__init__.py`.
    fn module_or_namespace_in(&self, dir: &str, dotted: &str, names: &[String]) -> Option<Target> {
        if let Some(t) = self.module_in(dir, dotted) {
            return Some(t);
        }
        let base = join(dir, &dotted.replace('.', "/"))?;
        let lands_on_file = names.iter().any(|n| {
            matches!(
                self.module_in(dir, &format!("{dotted}.{n}")),
                Some(Target::File(_))
            )
        });
        (lands_on_file && !base.is_empty() && self.ix.is_dir(&base)).then_some(Target::Dir(base))
    }

    /// Whether the top-level name is a local module file or regular package under `dir`.
    fn top_in(&self, dir: &str, top: &str) -> bool {
        join(dir, top).is_some_and(|b| {
            ["py", "pyi"].iter().any(|ext| {
                self.ix.is_file(&format!("{b}.{ext}"))
                    || self.ix.is_file(&format!("{b}/__init__.{ext}"))
            })
        })
    }

    /// The package dir a module target exposes submodules from.
    fn package_dir(&self, t: &Target) -> Option<String> {
        match t {
            Target::Dir(d) => Some(d.clone()),
            Target::File(f) => {
                let name = f.rsplit('/').next().unwrap_or(f);
                (name == "__init__.py" || name == "__init__.pyi").then(|| parent(f).to_string())
            }
            Target::External(_) => None,
        }
    }

    /// Resolve one statement. Returns `(specifier, outcome)` per distinct target.
    pub fn resolve(
        &self,
        importer: &str,
        module: &str,
        names: &[String],
    ) -> Vec<(String, Outcome)> {
        let dots = module.chars().take_while(|c| *c == '.').count();
        let rest = module.get(dots..).unwrap_or("");
        if dots > 0
            && rest.is_empty()
            && !names.is_empty()
            && let Some(out) = self.dot_import_without_init(importer, dots, module, names)
        {
            return out;
        }
        let outcome = if dots > 0 {
            self.relative(importer, dots, rest, names)
        } else {
            self.absolute(importer, rest, names)
        };
        let Outcome::Resolved { target, confidence } = outcome else {
            return vec![(module.to_string(), outcome)];
        };
        self.expand(module, target, confidence, names)
    }

    /// Fan `from M import a, b` out to submodules `M.a`, `M.b` when they exist.
    fn expand(
        &self,
        module: &str,
        target: Target,
        confidence: Confidence,
        names: &[String],
    ) -> Vec<(String, Outcome)> {
        let mut out: Vec<(String, Outcome)> = Vec::new();
        let mut push = |spec: String, t: Target| {
            if !out
                .iter()
                .any(|(_, o)| matches!(o, Outcome::Resolved { target, .. } if *target == t))
            {
                out.push((
                    spec,
                    Outcome::Resolved {
                        target: t,
                        confidence,
                    },
                ));
            }
        };
        let pkg = self.package_dir(&target);
        if names.is_empty() {
            push(module.to_string(), target.clone());
        }
        for n in names {
            let sub = pkg.as_deref().and_then(|d| self.module_in(d, n));
            match sub {
                // A submodule file or a namespace subpackage.
                Some(t @ (Target::File(_) | Target::Dir(_))) => {
                    let sep = if module.ends_with('.') { "" } else { "." };
                    push(format!("{module}{sep}{n}"), t);
                }
                _ => push(module.to_string(), target.clone()),
            }
        }
        out
    }

    /// The package dir `dots` leading dots refer to from `importer`; `None` if it would
    /// leave the repo.
    fn relative_base(importer: &str, dots: usize) -> Option<String> {
        let mut base = parent(importer).to_string();
        for _ in 1..dots {
            if base.is_empty() {
                return None;
            }
            base = parent(&base).to_string();
        }
        Some(base)
    }

    fn has_init(&self, dir: &str) -> bool {
        ["__init__.py", "__init__.pyi"]
            .iter()
            .any(|i| join(dir, i).is_some_and(|p| self.ix.is_file(&p)))
    }

    /// `from . import a, b` where the package dir has no `__init__.py` (a namespace dir, or
    /// the repo root): only submodules exist, so each name is its own import. Names that
    /// are not submodules are `not_found`; the dir itself is never a target. `None` when
    /// the dir has an `__init__.py` (the normal path handles it).
    fn dot_import_without_init(
        &self,
        importer: &str,
        dots: usize,
        module: &str,
        names: &[String],
    ) -> Option<Vec<(String, Outcome)>> {
        let Some(base) = Self::relative_base(importer, dots) else {
            return Some(vec![(
                module.to_string(),
                Outcome::not_found("relative import leaves the repo"),
            )]);
        };
        if self.has_init(&base) {
            return None;
        }
        let mut out: Vec<(String, Outcome)> = Vec::new();
        for n in names {
            let spec = format!("{module}{n}");
            let outcome = match self.module_in(&base, n) {
                Some(t) => Outcome::Resolved {
                    target: t,
                    confidence: Confidence::Exact,
                },
                None => Outcome::not_found(format!(
                    "no module {n} in {} (it has no __init__.py, so only submodules can be imported)",
                    show_dir(&base)
                )),
            };
            let duplicate = matches!(&outcome, Outcome::Resolved { target, .. }
                if out.iter().any(|(_, o)| matches!(o, Outcome::Resolved { target: t, .. } if t == target)));
            if !duplicate {
                out.push((spec, outcome));
            }
        }
        Some(out)
    }

    fn relative(&self, importer: &str, dots: usize, rest: &str, names: &[String]) -> Outcome {
        let Some(base) = Self::relative_base(importer, dots) else {
            return Outcome::not_found("relative import leaves the repo");
        };
        if !rest.is_empty() {
            return match self.module_in(&base, rest) {
                Some(t) => Outcome::Resolved {
                    target: t,
                    confidence: Confidence::Exact,
                },
                None => Outcome::not_found(format!("no module {rest} in {}", show_dir(&base))),
            };
        }
        // `from . import x` / `from . import *`: the package itself (its __init__, or a
        // namespace dir for `*`). Names without an __init__ are handled by
        // `dot_import_without_init`; the repo root is never a target.
        for init in ["__init__.py", "__init__.pyi"] {
            if let Some(p) = join(&base, init).filter(|p| self.ix.is_file(p)) {
                return Outcome::file(p, Confidence::Exact);
            }
        }
        if names.is_empty() && !base.is_empty() && self.ix.is_dir(&base) {
            return Outcome::Resolved {
                target: Target::Dir(base),
                confidence: Confidence::Exact,
            };
        }
        Outcome::not_found("relative import outside a package")
    }

    fn absolute(&self, importer: &str, dotted: &str, names: &[String]) -> Outcome {
        if dotted.is_empty() {
            return Outcome::not_found("empty module name");
        }
        let top = dotted.split('.').next().unwrap_or(dotted);
        let dir = parent(importer);
        let applicable: Vec<&Root> = self
            .roots
            .iter()
            .filter(|r| is_ancestor(&r.scope, dir))
            .collect();
        let mut local_top: Option<String> = None;

        if !self.packages.contains(dir) {
            let is_root = applicable.iter().any(|r| r.dir == dir);
            if let Some(t) = self.module_or_namespace_in(dir, dotted, names) {
                let confidence = if is_root {
                    Confidence::Resolved
                } else {
                    Confidence::Heuristic
                };
                return Outcome::Resolved {
                    target: t,
                    confidence,
                };
            }
            if self.top_in(dir, top) {
                local_top = Some(dir.to_string());
            }
        }
        for r in &applicable {
            if let Some(t) = self.module_or_namespace_in(&r.dir, dotted, names) {
                return Outcome::Resolved {
                    target: t,
                    confidence: Confidence::Resolved,
                };
            }
            if local_top.is_none() && self.top_in(&r.dir, top) {
                local_top = Some(r.dir.clone());
            }
        }
        if let Some(d) = local_top {
            return Outcome::not_found(format!(
                "local package {top} (in {}) has no module {dotted}",
                show_dir(&d)
            ));
        }
        if builtins::is_python_stdlib(top) {
            return Outcome::external(format!("python:{top}"), Confidence::Exact);
        }
        let mut others: Vec<Target> = Vec::new();
        for r in self
            .roots
            .iter()
            .filter(|r| r.project && !is_ancestor(&r.scope, dir))
        {
            if let Some(t) = self.module_or_namespace_in(&r.dir, dotted, names)
                && !others.contains(&t)
            {
                others.push(t);
            }
        }
        match others.as_slice() {
            [one] => Outcome::Resolved {
                target: one.clone(),
                confidence: Confidence::Heuristic,
            },
            [_, _, ..] => {
                let shown: Vec<String> = others
                    .iter()
                    .take(5)
                    .map(|t| match t {
                        Target::File(p) | Target::Dir(p) | Target::External(p) => p.clone(),
                    })
                    .collect();
                Outcome::ambiguous(format!(
                    "{dotted} exists in {} projects: {}{}",
                    others.len(),
                    shown.join(", "),
                    if others.len() > 5 { ", …" } else { "" }
                ))
            }
            [] => Outcome::external(format!("pypi:{top}"), Confidence::Resolved),
        }
    }
}

fn show_dir(d: &str) -> &str {
    if d.is_empty() { "the repo root" } else { d }
}
