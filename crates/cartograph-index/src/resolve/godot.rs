//! Godot resolution: `res://`, `uid://` and relative paths, global class names and autoloads.
//!
//! - `res://` maps to the directory holding the nearest `project.godot` above the file, so
//!   a repo can hold several projects. A file outside every project cannot use `res://`.
//! - A relative path (`load("enemy.gd")`) is relative to the file's directory.
//! - `uid://` resolves through the project's uid index: `*.uid` files (Godot 4.4+), the
//!   `uid` in the `[remap]` of `*.import` files, and the header `uid` of scenes and
//!   resources. An `ext_resource` with both resolves by path unless its uid names another
//!   file (Godot loads by uid first).
//! - Global names: `class_name` declarations and `[autoload]` names, per project. `extends
//!   Name` resolves to the declaring file, else to `external:godot:<Name>` for a Godot
//!   built-in class, else it is `not_found`. Other uses of a name become edges only when
//!   they hit the index.
//!
//! Every path reference becomes an `Import` (or `ImportExternal`) edge with an import site
//! whose specifier is `"{role} {path}"`. On top, [`Resolver::extras`] adds `Extends` edges
//! (script inheritance, inherited scenes), `Call` edges from a scene to the script that
//! handles one of its signal connections, and node metadata: the connections of each scene
//! (with the reason when the handler's script is unknown) and the entry points of each
//! `project.godot`.

use super::{FileImports, Index, Outcome, Target, join, parent};
use crate::godot::{PathKind, Ref, SignalTarget, text};
use crate::imports::Spec;
use cartograph_core::{Confidence, Edge, EdgeKind, NodeKind, UnresolvedReason, node_id};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Name of a Godot project file.
const PROJECT_FILE: &str = "project.godot";

/// Godot lookups over the whole repo.
pub struct Resolver<'a> {
    ix: &'a Index<'a>,
    /// Dirs that hold a `project.godot` (`""` for the repo root).
    projects: HashSet<&'a str>,
    /// (project, global name) → declaring files (`class_name`) or autoload targets.
    names: HashMap<(String, String), Vec<String>>,
    /// (project, `uid://…`) → files.
    uids: HashMap<(String, String), Vec<String>>,
    /// Scene path → its root node's script reference (`path`, `uid`), as written.
    root_scripts: HashMap<String, (String, Option<String>)>,
    /// Lower-cased path → indexed paths, for "differs only in case" details.
    lower: HashMap<String, Vec<&'a str>>,
}

fn godot_refs<'f>(f: &'f FileImports<'_>) -> impl Iterator<Item = (u32, &'f Ref)> {
    f.imports.iter().filter_map(|r| match &r.spec {
        Spec::Godot(g) => Some((r.line, g)),
        _ => None,
    })
}

impl<'a> Resolver<'a> {
    pub fn new(ix: &'a Index<'a>, files: &[FileImports<'_>]) -> Resolver<'a> {
        let projects: HashSet<&str> = ix
            .files
            .iter()
            .filter(|p| p.rsplit('/').next() == Some(PROJECT_FILE))
            .map(|p| parent(p))
            .collect();
        let mut r = Resolver {
            ix,
            projects,
            names: HashMap::new(),
            uids: HashMap::new(),
            root_scripts: HashMap::new(),
            lower: HashMap::new(),
        };
        if !files.iter().any(|f| crate::godot::supports(f.lang)) {
            return r;
        }
        for p in &ix.files {
            r.lower.entry(p.to_lowercase()).or_default().push(p);
        }
        let mut wants_uids = false;
        for f in files {
            // Files outside every project share the `""` key, as in the lookups.
            let project = r.project_of(f.path).unwrap_or("").to_string();
            for (_, g) in godot_refs(f) {
                match g {
                    Ref::DeclareClass(name) => {
                        r.names
                            .entry((project.clone(), name.clone()))
                            .or_default()
                            .push(f.path.to_string());
                    }
                    Ref::DeclareUid(uid) => {
                        r.uids
                            .entry((project.clone(), uid.clone()))
                            .or_default()
                            .push(f.path.to_string());
                    }
                    Ref::RootScript { path, uid } => {
                        r.root_scripts
                            .insert(f.path.to_string(), (path.clone(), uid.clone()));
                    }
                    Ref::Path { path, uid, .. } => {
                        wants_uids |= uid.is_some() || path.starts_with("uid://");
                    }
                    Ref::Autoload { path, .. } => {
                        wants_uids |= path.starts_with("uid://");
                    }
                    Ref::Signal(s) => {
                        wants_uids |= matches!(
                            &s.target,
                            SignalTarget::Script { uid: Some(_), .. }
                                | SignalTarget::Instance { uid: Some(_), .. }
                        );
                    }
                    _ => {}
                }
            }
        }
        if wants_uids {
            r.index_uid_files();
        }
        for v in r.uids.values_mut() {
            v.sort();
            v.dedup();
        }
        // Autoloads after uids: their paths may be uids.
        let mut autoloads = Vec::new();
        for f in files.iter().filter(|f| f.lang == "godot-project") {
            for (_, g) in godot_refs(f) {
                if let Ref::Autoload { name, path } = g
                    && let Outcome::Resolved {
                        target: Target::File(t),
                        ..
                    } = r.lookup(f.path, path, Confidence::Exact, false)
                {
                    autoloads.push(((parent(f.path).to_string(), name.clone()), t));
                }
            }
        }
        for (k, t) in autoloads {
            r.names.entry(k).or_default().push(t);
        }
        for v in r.names.values_mut() {
            v.sort();
            v.dedup();
        }
        r
    }

    /// Read every indexed `*.uid` and `*.import` file of a project into the uid index.
    fn index_uid_files(&mut self) {
        let ix = self.ix;
        let mut found = Vec::new();
        for p in &ix.files {
            let (target, uid) = if let Some(t) = p.strip_suffix(".uid") {
                let Some(text) = ix.read_config(p) else {
                    continue;
                };
                (t, text.trim().to_string())
            } else if let Some(t) = p.strip_suffix(".import") {
                let Some(text) = ix.read_config(p) else {
                    continue;
                };
                let doc = text::parse(&text);
                let uid = doc
                    .sections
                    .iter()
                    .filter(|s| s.tag == "remap")
                    .find_map(|s| s.prop("uid"))
                    .and_then(|p| text::unquote(&p.value));
                let Some(uid) = uid else { continue };
                (t, uid)
            } else {
                continue;
            };
            if !uid.starts_with("uid://") {
                continue;
            }
            let project = self.project_of(target).unwrap_or("");
            found.push(((project.to_string(), uid), target.to_string()));
        }
        for (k, t) in found {
            self.uids.entry(k).or_default().push(t);
        }
    }

    /// Dir of the nearest `project.godot` at or above `path`'s directory.
    fn project_of(&self, path: &str) -> Option<&'a str> {
        let mut dir = parent(path);
        loop {
            if let Some(p) = self.projects.get(dir) {
                return Some(p);
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent(dir);
        }
    }

    /// Resolve one Godot reference of `file`. Declarations and signals yield nothing here.
    pub fn resolve(&self, file: &str, r: &Ref) -> Vec<(String, Outcome)> {
        match r {
            Ref::Path {
                role,
                path,
                uid,
                kind,
            } => vec![(
                format!("{role} {path}"),
                self.path_outcome(file, path, uid.as_deref(), *kind),
            )],
            Ref::Autoload { name, path } => vec![(
                format!("autoload {name} {path}"),
                self.lookup(file, path, Confidence::Exact, false),
            )],
            Ref::Name { name, extends } => {
                let spec = if *extends {
                    format!("extends {name}")
                } else {
                    name.clone()
                };
                match self.name_outcome(file, name, *extends) {
                    Some(o) => vec![(spec, o)],
                    None => Vec::new(),
                }
            }
            Ref::Dynamic { role, text } => {
                let spec = if role == "string" {
                    text.clone()
                } else {
                    format!("{role}({text})")
                };
                vec![(
                    spec,
                    Outcome::Unresolved {
                        reason: UnresolvedReason::Dynamic,
                        detail: None,
                    },
                )]
            }
            Ref::DeclareClass(_) | Ref::DeclareUid(_) | Ref::RootScript { .. } | Ref::Signal(_) => {
                Vec::new()
            }
        }
    }

    fn name_outcome(&self, file: &str, name: &str, extends: bool) -> Option<Outcome> {
        let project = self.project_of(file).unwrap_or("");
        let hits = self
            .names
            .get(&(project.to_string(), name.to_string()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        match hits {
            [one] if one == file => None,
            [one] => Some(Outcome::file(one.clone(), Confidence::Resolved)),
            [] if extends && crate::godot::is_builtin_class(name) => Some(Outcome::external(
                format!("godot:{name}"),
                Confidence::Exact,
            )),
            [] if extends => Some(Outcome::not_found(format!(
                "no class_name {name} in this project and not a Godot {} built-in class",
                crate::godot::BUILTIN_VERSION
            ))),
            [] => None,
            many => Some(Outcome::ambiguous(format!(
                "{name} is declared by {}",
                many.join(", ")
            ))),
        }
    }

    /// Outcome of a path reference, using its uid when it names a file the path does not.
    fn path_outcome(&self, file: &str, path: &str, uid: Option<&str>, kind: PathKind) -> Outcome {
        let confidence = match kind {
            PathKind::Direct | PathKind::Extends => Confidence::Exact,
            PathKind::String => Confidence::Heuristic,
        };
        let by_path = self.lookup(file, path, confidence, kind == PathKind::String);
        let Some(uid) = uid else { return by_path };
        let by_uid = self.lookup(file, uid, Confidence::Resolved, false);
        match (&by_path, &by_uid) {
            (
                Outcome::Resolved {
                    target: Target::File(a),
                    ..
                },
                Outcome::Resolved {
                    target: Target::File(b),
                    ..
                },
            ) if a != b => by_uid,
            (Outcome::Resolved { .. }, _) => by_path,
            (_, Outcome::Resolved { .. }) => by_uid,
            (Outcome::Unresolved { reason, detail }, Outcome::Unresolved { detail: d2, .. }) => {
                Outcome::Unresolved {
                    reason: *reason,
                    detail: Some(format!(
                        "{}; {}",
                        detail.as_deref().unwrap_or(""),
                        d2.as_deref().unwrap_or("")
                    )),
                }
            }
        }
    }

    /// Resolve a `res://`, `uid://`, `user://` or relative path written in `file`. Directories
    /// count only when `allow_dir` (string mentions such as `"res://tests/"`).
    fn lookup(&self, file: &str, raw: &str, confidence: Confidence, allow_dir: bool) -> Outcome {
        // `res://a.tres::Resource_x` names a sub-resource of a.tres.
        let raw = raw.split("::").next().unwrap_or(raw).trim();
        if raw.starts_with("uid://") {
            let project = self.project_of(file).unwrap_or("");
            let hits = self
                .uids
                .get(&(project.to_string(), raw.to_string()))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            return match hits {
                [one] if self.ix.is_file(one) => Outcome::file(one.clone(), Confidence::Resolved),
                [one] => Outcome::not_found(format!("{raw} names {one}, which is not indexed")),
                [] => Outcome::not_found(format!(
                    "no .uid file, .import file or scene header in this project declares {raw}"
                )),
                many => Outcome::ambiguous(format!("{raw} is declared by {}", many.join(", "))),
            };
        }
        if raw.starts_with("user://") {
            return Outcome::not_found("user:// is the user data folder, outside the project");
        }
        let (base, rel) = if let Some(rel) = raw.strip_prefix("res://") {
            match self.project_of(file) {
                Some(p) => (p, rel),
                None => {
                    return Outcome::not_found(
                        "res:// path in a file outside every Godot project (no project.godot above it)",
                    );
                }
            }
        } else if raw.contains("://") {
            return Outcome::not_found("unsupported scheme");
        } else {
            (parent(file), raw)
        };
        let Some(target) = join(base, rel) else {
            return Outcome::not_found("path leaves the repo");
        };
        if self.ix.is_file(&target) {
            return Outcome::file(target, confidence);
        }
        if allow_dir && !target.is_empty() && self.ix.is_dir(&target) {
            return Outcome::Resolved {
                target: Target::Dir(target),
                confidence,
            };
        }
        if target.is_empty() {
            return Outcome::not_found("the project root is not a file");
        }
        if let Some(reason) = self.ix.skip_reason(&target) {
            return Outcome::not_found(format!("{target} was skipped ({reason})"));
        }
        if let Some(found) = self.lower.get(&target.to_lowercase()) {
            return Outcome::not_found(format!(
                "no file {target}; {} differs only in case",
                found.join(", ")
            ));
        }
        if self.ix.on_disk(&target).is_file() {
            return Outcome::not_found(format!(
                "{target} exists on disk but is not indexed (ignored)"
            ));
        }
        Outcome::not_found(format!("no file {target}"))
    }

    /// `Extends` and `Call` edges, and node metadata `(file path, meta)` for scenes with
    /// signal connections and for `project.godot` files.
    pub fn extras(&self, files: &[FileImports<'_>]) -> (Vec<Edge>, Vec<(String, Value)>) {
        let mut edges = Vec::new();
        let mut metas = Vec::new();
        for f in files.iter().filter(|f| crate::godot::supports(f.lang)) {
            let src = node_id(NodeKind::File, f.path);
            let mut connections = Vec::new();
            let mut entries = Vec::new();
            for (line, g) in godot_refs(f) {
                match g {
                    Ref::Path {
                        path,
                        uid,
                        kind: PathKind::Extends,
                        ..
                    } => {
                        if let Outcome::Resolved { target, confidence } =
                            self.path_outcome(f.path, path, uid.as_deref(), PathKind::Extends)
                        {
                            edges.push(Edge::new(
                                src.clone(),
                                target.node_id(),
                                EdgeKind::Extends,
                                confidence,
                            ));
                        }
                    }
                    Ref::Name {
                        name,
                        extends: true,
                    } => {
                        if let Some(Outcome::Resolved { target, confidence }) =
                            self.name_outcome(f.path, name, true)
                        {
                            edges.push(Edge::new(
                                src.clone(),
                                target.node_id(),
                                EdgeKind::Extends,
                                confidence,
                            ));
                        }
                    }
                    Ref::Path {
                        role, path, uid, ..
                    } if role == "main_scene" => {
                        let o = self.path_outcome(f.path, path, uid.as_deref(), PathKind::Direct);
                        entries.push(entry("main_scene", None, line, path, &o));
                    }
                    Ref::Autoload { name, path } => {
                        let o = self.lookup(f.path, path, Confidence::Exact, false);
                        entries.push(entry("autoload", Some(name), line, path, &o));
                    }
                    Ref::Signal(s) => {
                        let (script, reason) = match self.signal_script(f.path, &s.target, &s.to) {
                            Ok(p) => (Some(node_id(NodeKind::File, &p)), None),
                            Err(why) => (None, Some(why)),
                        };
                        if let Some(dst) = &script {
                            edges.push(Edge::new(
                                src.clone(),
                                dst.clone(),
                                EdgeKind::Call,
                                Confidence::Resolved,
                            ));
                        }
                        connections.push(json!({
                            "line": line,
                            "signal": s.signal,
                            "from": s.from,
                            "to": s.to,
                            "method": s.method,
                            "script": script,
                            "reason": reason,
                        }));
                    }
                    _ => {}
                }
            }
            let mut godot = BTreeMap::new();
            if !connections.is_empty() {
                godot.insert("connections", Value::Array(connections));
            }
            if !entries.is_empty() {
                godot.insert("entry_points", Value::Array(entries));
            }
            if !godot.is_empty() {
                metas.push((f.path.to_string(), json!({ "godot": godot })));
            }
        }
        (edges, metas)
    }

    /// Path of the script that handles a connection to node `to`, or why it is unknown.
    fn signal_script(
        &self,
        scene: &str,
        target: &SignalTarget,
        to: &str,
    ) -> Result<String, String> {
        let script = |file: &str, path: &str, uid: Option<&str>| match self.path_outcome(
            file,
            path,
            uid,
            PathKind::Direct,
        ) {
            Outcome::Resolved {
                target: Target::File(p),
                ..
            } => Ok(p),
            Outcome::Resolved { target, .. } => Err(format!(
                "script {path} resolves to {}, not a file",
                target.node_id()
            )),
            Outcome::Unresolved { detail, .. } => Err(format!(
                "script {path} not found: {}",
                detail.unwrap_or_default()
            )),
        };
        match target {
            SignalTarget::Script { path, uid } => script(scene, path, uid.as_deref()),
            SignalTarget::Instance { path, uid } => {
                let sub = match self.path_outcome(scene, path, uid.as_deref(), PathKind::Direct) {
                    Outcome::Resolved {
                        target: Target::File(p),
                        ..
                    } => p,
                    _ => return Err(format!("instanced scene {path} not found")),
                };
                match self.root_scripts.get(&sub) {
                    Some((p, u)) => script(&sub, p, u.as_deref()),
                    None => Err(format!("instanced scene {sub} has no root script")),
                }
            }
            SignalTarget::NoScript => Err(format!("node {to} has no script")),
            SignalTarget::UnknownNode => Err(format!(
                "node {to} is not in this scene (inside an instanced or inherited scene?)"
            )),
        }
    }
}

fn entry(kind: &str, name: Option<&str>, line: u32, path: &str, o: &Outcome) -> Value {
    let (target, reason) = match o {
        Outcome::Resolved { target, .. } => (Some(target.node_id()), None),
        Outcome::Unresolved { detail, .. } => (None, detail.clone()),
    };
    json!({
        "kind": kind,
        "name": name,
        "line": line,
        "path": path,
        "target": target,
        "reason": reason,
    })
}
