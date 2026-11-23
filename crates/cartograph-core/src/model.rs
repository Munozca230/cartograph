//! Graph model shared by the indexer, the store and every surface (CLI, web, MCP).
//!
//! Paths are always repo-relative with forward slashes, on every platform. The
//! repo root itself has the empty path `""`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Level of a node in the hierarchy repo → package → dir → file → symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Repo,
    Package,
    Dir,
    File,
    Symbol,
    /// A dependency outside the repo: `npm:<pkg>`, `node:<builtin>`, `pypi:<top-level>`,
    /// `python:<stdlib module>`. Its path is that string; it has no container.
    External,
}

/// Relationship carried by an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// Structural containment: repo → package, repo/dir → dir, repo/dir → file.
    Contains,
    /// Package → the manifest file that declares it.
    Manifest,
    /// File → File (or File → Dir for a Python namespace package). `weight` = number of
    /// import sites.
    Import,
    /// File → External. `weight` = number of import sites.
    #[serde(rename = "import_external")]
    ImportExternal,
    /// Derived: Dir → Dir and Package → Package. `weight` = number of distinct file → file
    /// `Import` edges crossing from the source to the destination.
    #[serde(rename = "depends_on")]
    DependsOn,
    Call,
    Extends,
    Implements,
    Route,
    Uses,
    /// Derived: File → File over a matched HTTP or IPC bridge (see [`crate::bridge`]).
    /// `weight` = number of bridged sites, confidence = the best of them. Not an import:
    /// cycles, layer rules and size measures never see it.
    Bridge,
}

/// How sure the indexer is about an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Known from the file system or a manifest, or from a precise index.
    Exact,
    /// Resolved by following language rules (e.g. an import path that maps to a file).
    Resolved,
    /// Best guess (e.g. a call matched by name only).
    Heuristic,
}

/// Why a file was not indexed. Every skipped path is recorded; nothing is dropped silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// A NUL byte appeared in the first 8 KB.
    Binary,
    /// Larger than the size limit (1 MB by default).
    TooLarge,
    /// A directory that is always excluded (`.git`, `.cartograph`, `node_modules`, `target`),
    /// the data dir, or the database files when the data dir is the repo root; a copy of a
    /// repository (a worktree); a folder of data or tool output (captures, logs, backups,
    /// caches; see `cartograph_index::noise`); the repo's notes file. `detail` says which
    /// when it is not one of the fixed names.
    ExcludedDir,
    /// A symbolic link (not followed).
    Symlink,
    /// The path is not valid Unicode and cannot be stored faithfully.
    InvalidPath,
    /// The file could not be read.
    ReadError,
    /// The directory walker reported an error for this path.
    WalkError,
    /// The file was read but could not be parsed (e.g. a malformed manifest). The file
    /// itself is still indexed; what failed is described in `detail`.
    ParseError,
    /// A minified bundle (`*.min.js`, or code whose lines are very long on average):
    /// generated, slow to parse, nothing to map.
    Minified,
}

/// Why an import could not be resolved to a file or an external package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedReason {
    /// The specifier looks local (relative, alias, workspace, local package) but no indexed
    /// file matches it.
    NotFound,
    /// The specifier is computed at run time (`require(x)`, `import(name)`).
    Dynamic,
    /// Several equally good targets exist (e.g. the same package name in two places).
    Ambiguous,
    /// A syntax error swallowed something that looks like an import, or the file did not
    /// parse at all.
    ParseError,
}

macro_rules! str_enum {
    ($ty:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self { $($ty::$variant => $s),+ }
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl FromStr for $ty {
            type Err = crate::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok($ty::$variant),)+
                    other => Err(crate::Error::UnknownVariant {
                        ty: stringify!($ty),
                        value: other.to_string(),
                    }),
                }
            }
        }
    };
}

str_enum!(NodeKind {
    Repo => "repo", Package => "package", Dir => "dir", File => "file", Symbol => "symbol",
    External => "external",
});
str_enum!(EdgeKind {
    Contains => "contains", Manifest => "manifest", Import => "import",
    ImportExternal => "import_external", DependsOn => "depends_on", Call => "call",
    Extends => "extends", Implements => "implements", Route => "route", Uses => "uses",
    Bridge => "bridge",
});
str_enum!(Confidence { Exact => "exact", Resolved => "resolved", Heuristic => "heuristic" });
str_enum!(SkipReason {
    Binary => "binary", TooLarge => "too_large", ExcludedDir => "excluded_dir",
    Symlink => "symlink", InvalidPath => "invalid_path", ReadError => "read_error",
    WalkError => "walk_error", ParseError => "parse_error", Minified => "minified",
});
str_enum!(UnresolvedReason {
    NotFound => "not_found", Dynamic => "dynamic", Ambiguous => "ambiguous",
    ParseError => "parse_error",
});

/// A node of the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Stable id derived from kind and path (see [`node_id`]).
    pub id: String,
    pub kind: NodeKind,
    /// Repo-relative path with forward slashes (`""` for the repo root).
    pub path: String,
    /// Display name: the last path segment, or the package name for packages.
    pub name: String,
    /// Detected language, for files.
    pub lang: Option<String>,
    /// Non-blank lines. For dirs and the repo: sum over contained files.
    pub loc: u64,
    /// Size in bytes. For dirs and the repo: sum over contained files.
    pub bytes: u64,
    /// BLAKE3 hex digest of the content, for files.
    pub hash: Option<String>,
    /// Kind-specific extra data (e.g. manifest details for packages).
    pub meta: serde_json::Value,
}

/// A directed edge of the graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub src: String,
    pub dst: String,
    pub kind: EdgeKind,
    pub confidence: Confidence,
    /// Multiplicity: 1 for structural edges; see [`EdgeKind`] for import and derived edges.
    #[serde(default = "one")]
    pub weight: u64,
}

fn one() -> u64 {
    1
}

impl Edge {
    /// An edge with weight 1.
    pub fn new(
        src: impl Into<String>,
        dst: impl Into<String>,
        kind: EdgeKind,
        confidence: Confidence,
    ) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            kind,
            confidence,
            weight: 1,
        }
    }
}

/// One import occurrence that resolved to a node (a file, a namespace-package dir or an
/// external). Edges collapse occurrences; sites keep every one with its line.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ImportSite {
    /// Repo-relative path of the importing file.
    pub file: String,
    /// 1-based line of the import statement or call.
    pub line: u32,
    /// The specifier as written (`./a`, `@/x`, `..pkg.mod`, `react`).
    pub specifier: String,
    /// Id of the node it resolved to.
    pub target: String,
}

/// One import occurrence that did not resolve. Every one is stored; nothing is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnresolvedImport {
    pub file: String,
    pub line: u32,
    /// The specifier as written; for dynamic imports, the argument expression (truncated).
    pub specifier: String,
    pub reason: UnresolvedReason,
    pub detail: Option<String>,
}

/// A path the indexer saw but did not index, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SkippedFile {
    pub path: String,
    pub reason: SkipReason,
    /// Free-form detail (size, error message…).
    pub detail: Option<String>,
}

/// A problem in a config file the resolvers read (a tsconfig `extends` that does not
/// exist, a bad workspace glob…). The file itself is indexed; this is not a skipped file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Repo-relative path of the config file.
    pub path: String,
    pub detail: String,
}

/// A whole indexed graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub skipped: Vec<SkippedFile>,
    #[serde(default)]
    pub imports: Vec<ImportSite>,
    #[serde(default)]
    pub unresolved: Vec<UnresolvedImport>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl Graph {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn nodes_of(&self, kind: NodeKind) -> impl Iterator<Item = &Node> {
        self.nodes.iter().filter(move |n| n.kind == kind)
    }

    /// Sort nodes, edges, skipped files and import records so two graphs of the same repo
    /// compare equal. Duplicate edges are merged (see [`merge_edges`]).
    pub fn normalize(&mut self) {
        self.nodes.sort_by(|a, b| a.id.cmp(&b.id));
        merge_edges(&mut self.edges);
        merge_skipped(&mut self.skipped);
        self.imports.sort();
        self.unresolved.sort();
        self.diagnostics.sort();
        self.diagnostics.dedup();
    }
}

/// Sort edges and merge those with the same `(src, dst, kind)`: weights add up and the most
/// certain confidence wins. Nothing is lost and the store's primary key holds.
pub fn merge_edges(edges: &mut Vec<Edge>) {
    edges.sort_by(|a, b| (&a.src, &a.dst, a.kind).cmp(&(&b.src, &b.dst, b.kind)));
    let mut out: Vec<Edge> = Vec::with_capacity(edges.len());
    for e in edges.drain(..) {
        match out.last_mut() {
            Some(last) if last.src == e.src && last.dst == e.dst && last.kind == e.kind => {
                last.weight += e.weight;
                last.confidence = last.confidence.min(e.confidence);
            }
            _ => out.push(e),
        }
    }
    *edges = out;
}

/// Sort skipped entries and merge those with the same `(path, reason)`, joining their
/// details with `"; "`, so nothing is lost and the store's primary key holds.
pub fn merge_skipped(skipped: &mut Vec<SkippedFile>) {
    skipped.sort();
    let mut out: Vec<SkippedFile> = Vec::with_capacity(skipped.len());
    for s in skipped.drain(..) {
        match out.last_mut() {
            Some(last) if last.path == s.path && last.reason == s.reason => {
                last.detail = match (last.detail.take(), s.detail) {
                    (Some(a), Some(b)) if a != b => Some(format!("{a}; {b}")),
                    (Some(a), _) => Some(a),
                    (None, b) => b,
                };
            }
            _ => out.push(s),
        }
    }
    *skipped = out;
}

/// Stable node id: `<kind>:<path>` (the repo node is `repo:`).
pub fn node_id(kind: NodeKind, path: &str) -> String {
    format!("{}:{}", kind.as_str(), path)
}

/// What a symbol definition is (N2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    /// A function inside a class, struct, trait or impl (its `container`).
    Method,
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    /// A type alias or a Go named type that is not a struct or interface.
    Type,
    /// A module-level constant.
    Const,
}

impl SymbolKind {
    /// Kinds whose definitions hold methods.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            SymbolKind::Class
                | SymbolKind::Interface
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Trait
        )
    }

    /// Functions and methods: what a call can be made from.
    pub fn is_callable(self) -> bool {
        matches!(self, SymbolKind::Function | SymbolKind::Method)
    }
}

/// Where a flow can start (see [`EntryPoint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A program's `main` (or Python's `if __name__ == "__main__":`).
    Main,
    /// An HTTP route handler.
    Http,
    /// A command-line command or subcommand.
    Cli,
    /// The main scene of a Godot project.
    GodotMainScene,
    /// A Godot autoload (singleton).
    GodotAutoload,
    /// An IPC channel handler of the main process (Electron `ipcMain.handle("channel", fn)`,
    /// `ipcMain.on`); the name is the channel as written.
    Ipc,
}

str_enum!(SymbolKind {
    Function => "function", Method => "method", Class => "class", Interface => "interface",
    Struct => "struct", Enum => "enum", Trait => "trait", Type => "type", Const => "const",
});
str_enum!(EntryKind {
    Main => "main", Http => "http", Cli => "cli", GodotMainScene => "godot_main_scene",
    GodotAutoload => "godot_autoload", Ipc => "ipc",
});

/// A symbol definition: a function, method, type or module-level constant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Symbol {
    /// Repo-relative path of the defining file.
    pub file: String,
    pub name: String,
    /// The class, struct, trait, interface or impl type a method belongs to.
    pub container: Option<String>,
    pub kind: SymbolKind,
    /// 1-based line of the name.
    pub line: u32,
    /// 1-based last line of the definition.
    pub end_line: u32,
    /// Exported or public: visible outside its file or module by the language's rules.
    pub exported: bool,
}

impl Symbol {
    /// `Container.name` for a method, else `name`.
    pub fn qualified(&self) -> String {
        match &self.container {
            Some(c) => format!("{c}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Stable id `sym:<path>#<qualified name>`. Not unique: overloads, two impls of one
    /// method name on the same type or a redefinition share an id, and every query treats
    /// them as one symbol.
    pub fn id(&self) -> String {
        symbol_id(&self.file, &self.qualified())
    }
}

/// `sym:<path>#<qualified name>`.
pub fn symbol_id(file: &str, qualified: &str) -> String {
    format!("sym:{file}#{qualified}")
}

/// Path and qualified name of a symbol id, if it is one.
///
/// Names can hold a `#` (a JS private `Model.#p`, a Rust raw `r#match`) and so can paths
/// (`src/c#/a.cs`), so neither the first nor the last `#` is always the separator. The
/// separator is the first `#` whose path part ends in a file name with an extension: what
/// follows a file's path is its name. A path whose file has no extension falls back to
/// the first `#`.
pub fn split_symbol_id(id: &str) -> Option<(&str, &str)> {
    split_path_name(id.strip_prefix("sym:")?)
}

/// `<path>#<qualified name>` → `(path, name)`, by the rule of [`split_symbol_id`].
pub fn split_path_name(s: &str) -> Option<(&str, &str)> {
    let mut first = None;
    for (i, _) in s.match_indices('#') {
        let path = &s[..i];
        if path.is_empty() {
            continue;
        }
        first.get_or_insert(i);
        let file = path.rsplit('/').next().unwrap_or(path);
        if file.contains('.') && !file.ends_with('.') {
            return Some((path, &s[i + 1..]));
        }
    }
    first.map(|i| (&s[..i], &s[i + 1..]))
}

/// One call site that resolved to a symbol. Calls that did not resolve are only counted
/// (see [`CallStats`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolCall {
    /// Repo-relative path of the calling file.
    pub file: String,
    /// 1-based line of the call.
    pub line: u32,
    /// Id of the function or method the call sits in; `None` at module level.
    pub caller: Option<String>,
    /// Id of the called symbol.
    pub target: String,
    /// `exact`: same file; `resolved`: through an import, a package or a project-wide
    /// class name; `heuristic`: by a name unique in the repo.
    pub confidence: Confidence,
}

/// Call sites of one file, by outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CallStats {
    pub file: String,
    pub exact: u32,
    pub resolved: u32,
    pub heuristic: u32,
    /// Several definitions fit and no rule picks one.
    pub ambiguous: u32,
    /// No definition in the repo fits (libraries, builtins, local variables, dynamic
    /// receivers).
    pub unresolved: u32,
}

impl CallStats {
    pub fn total(&self) -> u32 {
        self.exact + self.resolved + self.heuristic + self.ambiguous + self.unresolved
    }
}

/// A place where a flow starts: `main`, an HTTP route, a CLI command, a Godot main scene or
/// autoload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryPoint {
    /// Repo-relative path of the file that declares it.
    pub file: String,
    /// 1-based line of the declaration (0 for a whole file).
    pub line: u32,
    pub kind: EntryKind,
    /// What it is called: `main`, `GET /users/:id`, the command name, the autoload name.
    pub name: String,
    /// Id of the symbol that runs (`sym:…`) or of the file (`file:…`), when known.
    pub target: Option<String>,
}

/// A supertype of a class, struct, interface or trait (or of a GDScript script): a base
/// class, an implemented or extended interface, a trait a Rust type implements, a
/// supertrait, a Go embedded type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SuperType {
    /// Id of the subtype: a symbol (`sym:…`), or `file:…` for a GDScript script without
    /// `class_name`.
    pub sub: String,
    /// The supertype as written (`Base`, `ns.Base`, `fmt::Display`).
    pub name: String,
    /// Id of the supertype when it is a type of the repo (`sym:…`, or `file:…` for a
    /// script).
    pub target: Option<String>,
    /// How `target` was found: `exact` in the same file, `resolved` through an import or
    /// the package, `heuristic` by a name unique in the repo. `None` without a target.
    pub confidence: Option<Confidence>,
    /// Where it is declared: the subtype's file, or a Rust `impl Trait for Type` block.
    pub file: String,
    /// 1-based line of the declaration.
    pub line: u32,
}

/// The symbol layer (N2) of an indexed repo. Kept apart from [`Graph`]: file-level
/// dependencies and every finding built on them never see call data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbols {
    pub symbols: Vec<Symbol>,
    pub calls: Vec<SymbolCall>,
    /// One row per file with at least one call site.
    pub stats: Vec<CallStats>,
    pub entries: Vec<EntryPoint>,
    /// Supertypes of the repo's types.
    #[serde(default)]
    pub supers: Vec<SuperType>,
    /// The indexer computed this layer: a repo without symbol-bearing files has an empty
    /// layer that is indexed, while an index written before the layer existed is not.
    #[serde(default)]
    pub indexed: bool,
    /// What imported SCIP indexes (N3) changed here; empty without one.
    #[serde(default, skip_serializing_if = "crate::scip::ScipCoverage::is_empty")]
    pub scip: crate::scip::ScipCoverage,
    /// Both sides of every bridge found, matched or not (see [`crate::bridge`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<crate::bridge::BridgeEndpoint>,
    /// The bridges matched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridges: Vec<crate::bridge::Bridge>,
    /// Where string keys are defined, run, listed and used (see [`crate::key`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<crate::key::KeySite>,
    /// The indexer looked for string keys (an index written before them has none, which is
    /// not the same as a repo without any).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keys_indexed: bool,
}

impl Symbols {
    /// Sort everything so two runs over the same repo compare equal.
    pub fn normalize(&mut self) {
        self.symbols.sort_by(|a, b| {
            (&a.file, a.line, &a.container, &a.name, a.kind).cmp(&(
                &b.file,
                b.line,
                &b.container,
                &b.name,
                b.kind,
            ))
        });
        self.symbols.dedup();
        self.calls.sort();
        self.stats.sort();
        self.entries.sort();
        self.entries.dedup();
        // By subtype only (a stable sort): its supertypes stay in declared order.
        self.supers.sort_by(|a, b| a.sub.cmp(&b.sub));
        self.supers.dedup();
        self.scip.normalize();
        self.endpoints.sort();
        self.endpoints.dedup();
        self.bridges.sort();
        self.bridges.dedup();
        self.keys.sort();
        self.keys.dedup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_strings() {
        for k in [
            NodeKind::Repo,
            NodeKind::Package,
            NodeKind::Dir,
            NodeKind::File,
            NodeKind::Symbol,
        ] {
            assert_eq!(k.as_str().parse::<NodeKind>().unwrap(), k);
        }
        assert_eq!(
            "heuristic".parse::<Confidence>().unwrap(),
            Confidence::Heuristic
        );
        assert_eq!(
            "too_large".parse::<SkipReason>().unwrap(),
            SkipReason::TooLarge
        );
        assert!("nope".parse::<EdgeKind>().is_err());
        for k in [
            EdgeKind::Import,
            EdgeKind::ImportExternal,
            EdgeKind::DependsOn,
        ] {
            assert_eq!(k.as_str().parse::<EdgeKind>().unwrap(), k);
            // JSON and the database use the same spelling.
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        assert_eq!("external".parse::<NodeKind>().unwrap(), NodeKind::External);
        for r in [
            UnresolvedReason::NotFound,
            UnresolvedReason::Dynamic,
            UnresolvedReason::Ambiguous,
            UnresolvedReason::ParseError,
        ] {
            assert_eq!(r.as_str().parse::<UnresolvedReason>().unwrap(), r);
        }
    }

    #[test]
    fn duplicate_edges_merge_weights_and_keep_the_best_confidence() {
        let mut edges = vec![
            Edge::new("a", "b", EdgeKind::Import, Confidence::Resolved),
            Edge::new("a", "c", EdgeKind::Import, Confidence::Exact),
            Edge::new("a", "b", EdgeKind::Import, Confidence::Exact),
            Edge::new("a", "b", EdgeKind::DependsOn, Confidence::Exact),
        ];
        merge_edges(&mut edges);
        assert_eq!(edges.len(), 3);
        assert_eq!(
            (edges[0].dst.as_str(), edges[0].kind),
            ("b", EdgeKind::Import)
        );
        assert_eq!(edges[0].weight, 2);
        assert_eq!(edges[0].confidence, Confidence::Exact);
    }

    #[test]
    fn node_ids_are_kind_prefixed() {
        assert_eq!(node_id(NodeKind::Repo, ""), "repo:");
        assert_eq!(node_id(NodeKind::File, "src/a.rs"), "file:src/a.rs");
    }

    #[test]
    fn symbol_ids_split_back_into_path_and_name() {
        for (path, name) in [
            ("src/a.ts", "run"),
            // JS/TS private names, with and without a container.
            ("src/a.ts", "Model.#p"),
            ("src/a.ts", "#p"),
            // Rust raw identifiers and C++ operators.
            ("src/a.rs", "r#match"),
            ("src/v.cpp", "Vec.operator/"),
            // `#` in the path.
            ("src/c#/a.cs", "Program.Main"),
            ("src/a#b.ts", "Model.#p"),
            ("a#b.ts", "f"),
            // A file without an extension.
            ("bin/tool", "main"),
        ] {
            let id = symbol_id(path, name);
            assert_eq!(split_symbol_id(&id), Some((path, name)), "{id}");
        }
        assert_eq!(split_symbol_id("file:src/a.ts"), None);
        assert_eq!(split_symbol_id("sym:nohash"), None);
    }
}
