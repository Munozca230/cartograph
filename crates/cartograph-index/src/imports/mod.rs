//! N1 import extraction with tree-sitter queries.
//!
//! Each language has a `.scm` query in `queries/` (embedded with `include_str!`). The Rust
//! side only knows a handful of capture names, documented at the top of each query file:
//! `@import` (the statement or call, for the line), `@source` (a string literal holding a
//! TS/JS specifier), `@module` / `@from` / `@name` / `@future` (Python), and `@dynamic` (the
//! argument of `require()` / `import()` / `__import__()` / `importlib.import_module()`).
//! Adding a language whose imports fit these captures means adding a query and a
//! [`Family`] resolver, not new extraction code.
//!
//! Go, Java and C# queries capture only `@import` (an import statement, or a package /
//! namespace / type declaration the resolvers index); `dotted.rs` reads each node.
//!
//! Parsing runs inside the walker's rayon pool; every thread keeps its own parser per
//! grammar (`thread_local!`), and compiled queries are shared (`tree_sitter::Query` is
//! `Send + Sync`).

mod dotted;

use crate::grammars::Grammar;
pub use crate::spec::{MAX_SNIPPET_CHARS, RawImport, Spec};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

pub mod cfamily;
pub mod rust;
pub mod rust_spec;

/// Which resolver handles a language's imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// TypeScript, TSX, JavaScript, JSX (`.mjs`/`.cjs` are detected as `javascript`).
    Js,
    Python,
    /// Rust (`mod`, `use`, `extern crate`; see [`rust`]).
    Rust,
    /// C and C++ (`#include`; see [`cfamily`]).
    CFamily,
    Go,
    Java,
    CSharp,
}

impl Family {
    /// Family for a detected language name (see [`crate::lang::detect`]).
    pub fn of_lang(lang: &str) -> Option<Family> {
        match lang {
            "typescript" | "tsx" | "javascript" | "jsx" => Some(Family::Js),
            "python" => Some(Family::Python),
            "rust" => Some(Family::Rust),
            "c" | "cpp" => Some(Family::CFamily),
            "go" => Some(Family::Go),
            "java" => Some(Family::Java),
            "csharp" => Some(Family::CSharp),
            _ => None,
        }
    }
}

const JS_QUERY: &str = include_str!("queries/javascript.scm");
const TS_QUERY: &str = concat!(
    include_str!("queries/javascript.scm"),
    "\n",
    include_str!("queries/typescript.scm")
);
const PY_QUERY: &str = include_str!("queries/python.scm");
const RUST_QUERY: &str = include_str!("queries/rust.scm");
const C_QUERY: &str = include_str!("queries/c.scm");
const CPP_QUERY: &str = include_str!("queries/cpp.scm");
const GO_QUERY: &str = include_str!("queries/go.scm");
const JAVA_QUERY: &str = include_str!("queries/java.scm");
const CS_QUERY: &str = include_str!("queries/csharp.scm");

/// Bump when the extraction code changes what it returns for the same input. Part of
/// [`extractor_version`], the extraction-cache key. 2: TypeScript type-only import marks;
/// 3: the payload is an [`Extract`] (imports and symbols); 4: only whole-statement type
/// imports are type-only; 5: Rust paths in code ([`rust::RustSpec::Path`]).
pub const EXTRACTOR_REVISION: u32 = 5;

/// Resolved `name@version` of every tree-sitter crate, from `Cargo.lock` (see `build.rs`).
pub const GRAMMAR_VERSIONS: &str = env!("CARTOGRAPH_GRAMMAR_VERSIONS");

/// Version of the extraction, the extraction-cache key. It combines:
/// - [`EXTRACTOR_REVISION`];
/// - this crate's version;
/// - the resolved version of every tree-sitter crate ([`GRAMMAR_VERSIONS`]), so a
///   `cargo update` of a grammar invalidates the cache even though this crate's version
///   stays the same;
/// - the ABI version of each grammar used for imports;
/// - a hash of every query, import and symbol ones alike, so editing a `.scm` file
///   invalidates the cache too;
/// - the symbol extractor's revision ([`crate::symbols::REVISION`]).
///
/// None of these needs a manual bump, apart from the revisions when Rust code changes.
pub fn extractor_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let mut h = blake3::Hasher::new();
        for q in [
            JS_QUERY, TS_QUERY, PY_QUERY, RUST_QUERY, C_QUERY, CPP_QUERY, GO_QUERY, JAVA_QUERY,
            CS_QUERY,
        ]
        .iter()
        .chain(crate::symbols::QUERIES.iter())
        {
            h.update(q.as_bytes());
            h.update(&[0]);
        }
        h.update(format!("sym{}", crate::symbols::REVISION).as_bytes());
        let hex = h.finalize().to_hex();
        let abis: Vec<String> = [
            Grammar::TypeScript,
            Grammar::Tsx,
            Grammar::JavaScript,
            Grammar::Python,
            Grammar::Rust,
            Grammar::C,
            Grammar::Cpp,
            Grammar::Go,
            Grammar::Java,
            Grammar::CSharp,
        ]
        .iter()
        .map(|g| g.abi_version().to_string())
        .collect();
        format!(
            "r{EXTRACTOR_REVISION}-v{}-q{}-abi{}-{}-{GRAMMAR_VERSIONS}",
            env!("CARGO_PKG_VERSION"),
            &hex.as_str()[..16],
            abis.join("."),
            crate::godot::extractor_tag()
        )
    })
}

/// Whether [`extract`] does anything for `lang` (and so whether its result is worth caching).
pub fn supports(lang: &str) -> bool {
    crate::godot::supports(lang)
        || (Grammar::for_lang(lang).is_some() && Family::of_lang(lang).is_some())
}

/// Query source for a grammar, if import extraction supports it.
pub fn query_source(grammar: Grammar) -> Option<&'static str> {
    match grammar {
        Grammar::TypeScript | Grammar::Tsx => Some(TS_QUERY),
        Grammar::JavaScript => Some(JS_QUERY),
        Grammar::Python => Some(PY_QUERY),
        Grammar::Rust => Some(RUST_QUERY),
        Grammar::C => Some(C_QUERY),
        Grammar::Cpp => Some(CPP_QUERY),
        Grammar::Go => Some(GO_QUERY),
        Grammar::Java => Some(JAVA_QUERY),
        Grammar::CSharp => Some(CS_QUERY),
        _ => None,
    }
}

/// Compiled query for a grammar (compiled once, shared by every thread).
pub fn query(grammar: Grammar) -> Result<&'static Query, String> {
    static TS: OnceLock<Result<Query, String>> = OnceLock::new();
    static TSX: OnceLock<Result<Query, String>> = OnceLock::new();
    static JS: OnceLock<Result<Query, String>> = OnceLock::new();
    static PY: OnceLock<Result<Query, String>> = OnceLock::new();
    static RUST: OnceLock<Result<Query, String>> = OnceLock::new();
    static C: OnceLock<Result<Query, String>> = OnceLock::new();
    static CPP: OnceLock<Result<Query, String>> = OnceLock::new();
    static GO: OnceLock<Result<Query, String>> = OnceLock::new();
    static JAVA: OnceLock<Result<Query, String>> = OnceLock::new();
    static CS: OnceLock<Result<Query, String>> = OnceLock::new();
    let (cell, source) = match (grammar, query_source(grammar)) {
        (Grammar::TypeScript, Some(s)) => (&TS, s),
        (Grammar::Tsx, Some(s)) => (&TSX, s),
        (Grammar::JavaScript, Some(s)) => (&JS, s),
        (Grammar::Python, Some(s)) => (&PY, s),
        (Grammar::Rust, Some(s)) => (&RUST, s),
        (Grammar::C, Some(s)) => (&C, s),
        (Grammar::Cpp, Some(s)) => (&CPP, s),
        (Grammar::Go, Some(s)) => (&GO, s),
        (Grammar::Java, Some(s)) => (&JAVA, s),
        (Grammar::CSharp, Some(s)) => (&CS, s),
        _ => return Err(format!("no import query for {}", grammar.name())),
    };
    cell.get_or_init(|| {
        Query::new(&grammar.language(), source)
            .map_err(|e| format!("import query for {}: {e}", grammar.name()))
    })
    .as_ref()
    .map_err(Clone::clone)
}

thread_local! {
    static PARSERS: RefCell<HashMap<Grammar, Parser>> = RefCell::new(HashMap::new());
}

/// Parse with this thread's parser for `grammar`.
fn parse(grammar: Grammar, source: &[u8]) -> Result<Tree, String> {
    PARSERS.with(|cell| {
        let mut parsers = cell
            .try_borrow_mut()
            .map_err(|_| "parser already in use on this thread".to_string())?;
        let parser = match parsers.entry(grammar) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => v.insert(
                grammar
                    .parser()
                    .map_err(|e| format!("grammar {}: {e}", grammar.name()))?,
            ),
        };
        parser
            .parse(source, None)
            .ok_or_else(|| format!("tree-sitter returned no tree ({})", grammar.name()))
    })
}

/// Extract the imports of a file. Languages without import support yield nothing. Never
/// fails: a file that cannot be parsed yields one [`Spec::ParseError`] at line 0.
pub fn extract(lang: &str, source: &[u8]) -> Vec<RawImport> {
    extract_file(lang, source).imports
}

/// What extraction keeps of one file, from a single parse: its raw imports and its symbol
/// data (N2). This is the extraction-cache payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extract {
    #[serde(rename = "i", default)]
    pub imports: Vec<RawImport>,
    /// `None` for languages without symbols (Godot scenes) and for files that did not parse.
    #[serde(rename = "s", default, skip_serializing_if = "Option::is_none")]
    pub code: Option<crate::symbols::Code>,
}

/// The cache payload for `imports` and `code`, as [`Extract`] serializes it (without
/// cloning them).
pub fn extract_json(
    imports: &[RawImport],
    code: Option<&crate::symbols::Code>,
) -> serde_json::Result<String> {
    #[derive(Serialize)]
    struct Borrowed<'a> {
        #[serde(rename = "i")]
        imports: &'a [RawImport],
        #[serde(rename = "s", skip_serializing_if = "Option::is_none")]
        code: Option<&'a crate::symbols::Code>,
    }
    serde_json::to_string(&Borrowed { imports, code })
}

/// Extract imports and symbols of a file, parsing it once. Never fails: a file that cannot
/// be parsed yields one [`Spec::ParseError`] at line 0 and no symbols.
pub fn extract_file(lang: &str, source: &[u8]) -> Extract {
    let parse_error = |e: String| Extract {
        imports: vec![RawImport {
            line: 0,
            spec: Spec::ParseError(e),
        }],
        code: None,
    };
    if lang == "gdscript" {
        return match crate::godot::gdscript::parse(source) {
            Ok(tree) => Extract {
                imports: crate::godot::gdscript::extract_tree(&tree, source),
                code: symbols_of(Grammar::GdScript, &tree, source),
            },
            Err(e) => parse_error(e),
        };
    }
    if let Some(v) = crate::godot::extract(lang, source) {
        return Extract {
            imports: v,
            code: None,
        };
    }
    let (Some(grammar), Some(family)) = (Grammar::for_lang(lang), Family::of_lang(lang)) else {
        return Extract::default();
    };
    let tree = match parse(grammar, source) {
        Ok(t) => t,
        Err(e) => return parse_error(e),
    };
    let result = match family {
        Family::Rust => rust::extract(grammar, &tree, source),
        Family::CFamily => cfamily::extract(grammar, &tree, source),
        _ => extract_with(grammar, family, &tree, source),
    };
    match result {
        Ok(imports) => Extract {
            imports,
            code: symbols_of(grammar, &tree, source),
        },
        Err(e) => parse_error(e),
    }
}

/// Symbol data of a parsed file; a query error is logged and yields none.
fn symbols_of(grammar: Grammar, tree: &Tree, source: &[u8]) -> Option<crate::symbols::Code> {
    match crate::symbols::extract(grammar, tree, source) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(grammar = grammar.name(), error = %e, "symbol extraction failed");
            None
        }
    }
}

/// Captures of one `@import` node, merged over every match that hit it.
struct Group<'t> {
    stmt: Node<'t>,
    sources: Vec<Node<'t>>,
    modules: Vec<Node<'t>>,
    from: Option<Node<'t>>,
    names: Vec<Node<'t>>,
    future: bool,
    dynamic: Vec<Node<'t>>,
}

fn push_unique<'t>(v: &mut Vec<Node<'t>>, n: Node<'t>) {
    if !v.iter().any(|x| x.id() == n.id()) {
        v.push(n);
    }
}

fn extract_with(
    grammar: Grammar,
    family: Family,
    tree: &Tree,
    src: &[u8],
) -> Result<Vec<RawImport>, String> {
    let query = query(grammar)?;
    let capture_names = query.capture_names();

    let mut groups: BTreeMap<(usize, usize), Group<'_>> = BTreeMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let name_of = |i: u32| capture_names.get(i as usize).copied().unwrap_or("");
        let Some(stmt) = m
            .captures()
            .iter()
            .find(|c| name_of(c.index) == "import")
            .map(|c| c.node)
        else {
            continue;
        };
        let g = groups
            .entry((stmt.start_byte(), stmt.id()))
            .or_insert_with(|| Group {
                stmt,
                sources: Vec::new(),
                modules: Vec::new(),
                from: None,
                names: Vec::new(),
                future: false,
                dynamic: Vec::new(),
            });
        for c in m.captures() {
            match name_of(c.index) {
                "source" => push_unique(&mut g.sources, c.node),
                "module" => push_unique(&mut g.modules, c.node),
                "from" => g.from = Some(c.node),
                "name" => push_unique(&mut g.names, c.node),
                "future" => g.future = true,
                "dynamic" => push_unique(&mut g.dynamic, c.node),
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    for g in groups.values_mut() {
        let line = line_of(g.stmt);
        if matches!(family, Family::Go | Family::Java | Family::CSharp) {
            // One statement or declaration per `@import`; see `dotted.rs`.
            out.extend(
                dotted::specs(g.stmt, src)
                    .into_iter()
                    .map(|spec| RawImport { line, spec }),
            );
            continue;
        }
        let ts = matches!(grammar, Grammar::TypeScript | Grammar::Tsx);
        let mut push = |spec| out.push(RawImport { line, spec });
        g.sources.sort_by_key(Node::start_byte);
        let type_only = ts && type_only_statement(g.stmt);
        for s in &g.sources {
            match string_literal(*s, src) {
                Some(lit) if type_only => push(Spec::TypePath(lit)),
                Some(lit) => push(Spec::Path(lit)),
                None => push(Spec::Dynamic(snippet(*s, src))),
            }
        }
        g.modules.sort_by_key(Node::start_byte);
        for m in &g.modules {
            push(Spec::Python {
                module: compact(*m, src),
                names: Vec::new(),
            });
        }
        if g.future {
            push(Spec::Python {
                module: "__future__".into(),
                names: Vec::new(),
            });
        } else if let Some(from) = g.from {
            g.names.sort_by_key(Node::start_byte);
            push(Spec::Python {
                module: compact(from, src),
                names: g.names.iter().map(|n| compact(*n, src)).collect(),
            });
        }
        if !g.dynamic.is_empty() {
            g.dynamic.sort_by_key(Node::start_byte);
            // First real argument (comments such as `/* webpackChunkName */` are skipped).
            let spec = match g.dynamic.iter().find(|n| n.kind() != "comment") {
                Some(arg) => match (string_literal(*arg, src), family) {
                    (Some(lit), Family::Python) => Spec::Python {
                        module: lit,
                        names: Vec::new(),
                    },
                    // Not reached: these families have their own extractors.
                    (Some(lit), Family::Rust | Family::CFamily) => Spec::Dynamic(lit),
                    // `let a: import("./a").A` names a type, not a module to load.
                    (Some(lit), _) if ts && in_type_position(g.stmt) => Spec::TypePath(lit),
                    (Some(lit), _) => Spec::Path(lit),
                    (None, _) => Spec::Dynamic(snippet(*arg, src)),
                },
                None => Spec::Dynamic(snippet(g.stmt, src)),
            };
            push(spec);
        }
    }

    if tree.root_node().has_error() {
        let covered: Vec<std::ops::Range<usize>> =
            groups.values().map(|g| g.stmt.byte_range()).collect();
        for err in outermost_errors(tree) {
            if let Some(at) = uncovered_keyword(err, src, family, &covered) {
                let line = line_at(src, at);
                let text = String::from_utf8_lossy(src.get(at..err.end_byte()).unwrap_or(&[]))
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(MAX_SNIPPET_CHARS)
                    .collect();
                out.push(RawImport {
                    line,
                    spec: Spec::ParseError(text),
                });
            }
        }
        out.sort_by_key(|r| r.line);
    }
    Ok(out)
}

/// Whether a node has the `type` (or Flow's `typeof`) keyword as a direct child. The keyword
/// is an anonymous node; `import type from './t'` (a default import named `type`) is a named
/// identifier and does not count.
fn has_type_keyword(n: Node<'_>) -> bool {
    let mut c = n.walk();
    n.children(&mut c)
        .any(|k| !k.is_named() && matches!(k.kind(), "type" | "typeof"))
}

/// A TypeScript import or re-export that only brings types, as a whole statement:
/// `import type …`, `import type x = require(…)`, `export type { … } from`.
///
/// A list whose specifiers are all `type` (`import { type A, type B } from './x'`,
/// `export { type H } from './h'`) does not count: under `verbatimModuleSyntax` or
/// `preserveValueImports` it compiles to `import {} from './x'`, which still loads the
/// module. Reading it as a runtime dependency is the conservative choice.
fn type_only_statement(stmt: Node<'_>) -> bool {
    // `import x = require('…')`: the keyword sits on the statement around the clause.
    let stmt = match (stmt.kind(), stmt.parent()) {
        ("import_require_clause", Some(p)) => p,
        _ => stmt,
    };
    matches!(stmt.kind(), "import_statement" | "export_statement") && has_type_keyword(stmt)
}

/// Nodes inside which everything is a type (TypeScript).
fn is_type_context(kind: &str) -> bool {
    kind.ends_with("_type")
        || matches!(
            kind,
            "type_annotation"
                | "opting_type_annotation"
                | "omitting_type_annotation"
                | "adding_type_annotation"
                | "asserts_annotation"
                | "type_predicate_annotation"
                | "type_query"
                | "type_arguments"
                | "type_parameters"
                | "type_alias_declaration"
                | "interface_declaration"
                | "implements_clause"
                | "extends_type_clause"
                | "constraint"
        )
}

/// Whether an `import("…")` call sits in a type position: inside a type annotation, a
/// `typeof` query, a type alias or interface, or the type side of `x as T` /
/// `x satisfies T`. Types never hold code that runs, so any such ancestor settles it.
fn in_type_position(call: Node<'_>) -> bool {
    let mut child = call;
    while let Some(p) = child.parent() {
        if is_type_context(p.kind()) {
            return true;
        }
        if matches!(p.kind(), "as_expression" | "satisfies_expression")
            && p.named_child(0)
                .is_some_and(|first| first.id() != child.id())
        {
            return true;
        }
        child = p;
    }
    false
}

fn line_of(n: Node<'_>) -> u32 {
    u32::try_from(n.start_position().row + 1).unwrap_or(u32::MAX)
}

fn line_at(src: &[u8], byte: usize) -> u32 {
    let newlines = src
        .get(..byte)
        .map_or(0, |b| b.iter().filter(|&&c| c == b'\n').count());
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}

fn text(n: Node<'_>, src: &[u8]) -> String {
    String::from_utf8_lossy(src.get(n.byte_range()).unwrap_or(&[])).into_owned()
}

/// Node text with all whitespace removed (`a . b` → `a.b`, line continuations too).
fn compact(n: Node<'_>, src: &[u8]) -> String {
    text(n, src)
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\\')
        .collect()
}

/// Node text on one line, truncated to [`MAX_SNIPPET_CHARS`].
fn snippet(n: Node<'_>, src: &[u8]) -> String {
    let t = text(n, src);
    let one_line: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(MAX_SNIPPET_CHARS).collect()
}

/// Value of a string literal without interpolation: a JS `'…'`/`"…"` string, a template
/// string without `${}`, or a Python string without f-string interpolation. `None` for
/// anything else.
fn string_literal(n: Node<'_>, src: &[u8]) -> Option<String> {
    if !matches!(n.kind(), "string" | "template_string") {
        return None;
    }
    let mut out = String::new();
    let mut cursor = n.walk();
    for child in n.named_children(&mut cursor) {
        match child.kind() {
            "string_fragment" | "string_content" | "escape_sequence" => {
                out.push_str(&text(child, src));
            }
            "string_start" | "string_end" => {}
            _ => return None,
        }
    }
    Some(out)
}

/// ERROR nodes that are not inside another ERROR node.
fn outermost_errors(tree: &Tree) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        let descend = if node.is_error() {
            out.push(node);
            false
        } else {
            node.has_error()
        };
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// Byte offset of the first import keyword inside `err` that is not part of an import
/// already extracted.
fn uncovered_keyword(
    err: Node<'_>,
    src: &[u8],
    family: Family,
    covered: &[std::ops::Range<usize>],
) -> Option<usize> {
    let keywords: &[&str] = match family {
        Family::Js => &["import", "require"],
        Family::Python | Family::Go | Family::Java => &["import"],
        Family::Rust => &["use", "mod"],
        Family::CFamily => &["#include"],
        Family::CSharp => &["using"],
    };
    let range = err.byte_range();
    let body = src.get(range.clone())?;
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let literals = comment_and_string_ranges(err);
    let mut best: Option<usize> = None;
    for kw in keywords {
        let kw = kw.as_bytes();
        let mut i = 0;
        while i + kw.len() <= body.len() {
            if body.get(i..i + kw.len()) == Some(kw) {
                let before = i.checked_sub(1).and_then(|j| body.get(j)).copied();
                let after = body.get(i + kw.len()).copied();
                let at = range.start + i;
                if !before.is_some_and(is_ident)
                    && !after.is_some_and(is_ident)
                    && !covered.iter().any(|r| r.contains(&at))
                    && !literals.iter().any(|r| r.contains(&at))
                    && in_import_position(family, src, at, at + kw.len())
                {
                    best = Some(best.map_or(at, |b| b.min(at)));
                    break;
                }
            }
            i += 1;
        }
    }
    best
}

/// Byte ranges of comment and string nodes inside `err` (a keyword there is text, not an
/// import).
fn comment_and_string_ranges(err: Node<'_>) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut cursor = err.walk();
    loop {
        let node = cursor.node();
        let kind = node.kind();
        if node.id() != err.id() && (kind.contains("comment") || kind.contains("string")) {
            out.push(node.byte_range());
        } else if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.node().id() == err.id() {
                return out;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// Whether a keyword at `start..end` sits where an import statement can start, for the
/// families whose keyword also starts other code:
/// - Rust `use` / `mod`: first on its line, after nothing but whitespace, a visibility
///   (`pub`, `pub(crate)`) or attributes (`#[cfg(test)]`). This also skips text in a
///   comment or string the grammar did not recover as a node.
/// - C# `using`: a directive, not a `using` statement or declaration. The keyword is first
///   on its line (after `global`, if any), is not followed by `var` or `(`, and the text up
///   to the next `;` has no `(` (`using Stream s = Open();`).
fn in_import_position(family: Family, src: &[u8], start: usize, end: usize) -> bool {
    let line_start = src
        .get(..start)
        .and_then(|b| b.iter().rposition(|&c| c == b'\n'))
        .map_or(0, |p| p + 1);
    let prefix = String::from_utf8_lossy(src.get(line_start..start).unwrap_or(&[]));
    let prefix = prefix.trim();
    match family {
        Family::Rust => prefix.is_empty() || prefix.starts_with("pub") || prefix.ends_with(']'),
        Family::CSharp => {
            if !(prefix.is_empty() || prefix == "global") {
                return false;
            }
            let rest = src.get(end..).unwrap_or(&[]);
            let stmt_end = rest.iter().position(|&c| c == b';').unwrap_or(rest.len());
            let stmt = String::from_utf8_lossy(rest.get(..stmt_end).unwrap_or(&[]));
            let next = stmt.split_whitespace().next().unwrap_or("");
            !(next == "var" || next.starts_with("var(") || stmt.contains('('))
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(lang: &str, src: &str) -> Vec<(u32, Spec)> {
        extract(lang, src.as_bytes())
            .into_iter()
            .map(|r| (r.line, r.spec))
            .collect()
    }

    fn path(s: &str) -> Spec {
        Spec::Path(s.into())
    }

    fn py(m: &str, names: &[&str]) -> Spec {
        Spec::Python {
            module: m.into(),
            names: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn every_query_compiles_against_its_grammars() {
        for g in Grammar::ALL {
            if query_source(g).is_some() {
                query(g).unwrap();
            }
        }
    }

    const TS: &str = "import x from 'a';\nimport 'b';\nimport type {T} from 'c';\n\
        export * from './d';\nexport {e} from \"./e\";\nexport type {F} from './f';\n\
        const g = require('g');\nimport('h');\nrequire(`t`);\nrequire(`t${x}`);\n\
        import(name);\nexport * as ns from './j';\nimport(/* webpackChunkName: \"k\" */ './k');\n\
        function f() { return require('./inner'); }\n";

    #[test]
    fn typescript_and_javascript_forms() {
        let want_common = vec![
            (1, path("a")),
            (2, path("b")),
            (3, path("c")),
            (4, path("./d")),
            (5, path("./e")),
            (6, path("./f")),
            (7, path("g")),
            (8, path("h")),
            (9, path("t")),
            (10, Spec::Dynamic("`t${x}`".into())),
            (11, Spec::Dynamic("name".into())),
            (12, path("./j")),
            (13, path("./k")),
            (14, path("./inner")),
        ];
        // `import type` and `export type` are type-only in TypeScript.
        let mut want_ts = want_common.clone();
        want_ts[2].1 = Spec::TypePath("c".into());
        want_ts[5].1 = Spec::TypePath("./f".into());
        assert_eq!(specs("typescript", TS), want_ts);
        assert_eq!(specs("tsx", TS), want_ts);
        // JS has no `import type`; the grammar reports an ERROR around `type` but the
        // specifiers are still extracted and no parse_error is invented for them.
        assert_eq!(specs("javascript", TS), want_common);
        assert_eq!(
            specs("typescript", "import fs = require('fs');\n"),
            vec![(1, path("fs"))]
        );
    }

    #[test]
    fn typescript_type_only_imports() {
        let src = "import type { A } from './a';\nimport { type B, type C } from './bc';\n\
            import { type D, e } from './de';\nimport type F from './f';\nimport type from './t';\n\
            import {} from './empty';\nexport type { G } from './g';\nexport { type H } from './h';\n\
            export { I } from './i';\nlet j: import('./j').J;\ntype K = typeof import('./k');\n\
            const l = x as import('./l').L;\nconst m = await import('./m');\n\
            import n = require('./n');\nimport type o = require('./o');\n\
            interface P { p: import('./p').P }\n\
            function q(a: import('./q').Q): import('./r').R { return import('./s'); }\n\
            import * as ns from './ns';\nimport def, { type U } from './du';\n";
        let tp = |s: &str| Spec::TypePath(s.into());
        let want = vec![
            (1, tp("./a")),
            // Every specifier `type` but no `import type`: under `verbatimModuleSyntax` or
            // `preserveValueImports` it compiles to `import {} from './bc'`, which loads it.
            (2, path("./bc")),
            (3, path("./de")),
            (4, tp("./f")),
            (5, path("./t")),
            (6, path("./empty")),
            (7, tp("./g")),
            (8, path("./h")),
            (9, path("./i")),
            (10, tp("./j")),
            (11, tp("./k")),
            (12, tp("./l")),
            (13, path("./m")),
            (14, path("./n")),
            (15, tp("./o")),
            (16, tp("./p")),
            (17, tp("./q")),
            (17, tp("./r")),
            (17, path("./s")),
            (18, path("./ns")),
            (19, path("./du")),
        ];
        assert_eq!(specs("typescript", src), want);
        assert_eq!(specs("tsx", src), want);
    }

    #[test]
    fn python_forms() {
        let src = "import a.b\nimport a as b, c.d as e\nfrom a.b import c, d as e\n\
            from . import x\nfrom ..pkg.mod import (y,\n    z)\nfrom .. import *\n\
            from __future__ import annotations\ndef f():\n    import os\ntry:\n    import ujson\n\
            except ImportError:\n    pass\nimportlib.import_module('m.n')\n__import__(name)\n";
        assert_eq!(
            specs("python", src),
            vec![
                (1, py("a.b", &[])),
                (2, py("a", &[])),
                (2, py("c.d", &[])),
                (3, py("a.b", &["c", "d"])),
                (4, py(".", &["x"])),
                (5, py("..pkg.mod", &["y", "z"])),
                (7, py("..", &[])),
                (8, py("__future__", &[])),
                (10, py("os", &[])),
                (12, py("ujson", &[])),
                (15, py("m.n", &[])),
                (16, Spec::Dynamic("name".into())),
            ]
        );
    }

    #[test]
    fn crlf_sources_keep_line_numbers() {
        let src = "import a\r\n\r\nfrom b import c\r\n";
        assert_eq!(
            specs("python", src),
            vec![(1, py("a", &[])), (3, py("b", &["c"]))]
        );
    }

    #[test]
    fn syntax_errors_around_imports_are_reported() {
        let got = specs(
            "typescript",
            "import b from './b';\nimport { a from './a';\n",
        );
        assert!(
            got.iter().any(|(_, s)| matches!(s, Spec::ParseError(_))),
            "{got:?}"
        );
        assert!(got.contains(&(1, path("./b"))), "{got:?}");
        // An error far from any import keyword is not an unresolved import.
        let got = specs("python", "import os\ndef f(:\n    pass\n");
        assert_eq!(got, vec![(1, py("os", &[]))]);
    }

    /// Whether any spec is a parse error.
    fn has_parse_error(got: &[(u32, Spec)]) -> bool {
        got.iter().any(|(_, s)| matches!(s, Spec::ParseError(_)))
    }

    #[test]
    fn rust_keywords_in_comments_and_strings_inside_errors_are_not_imports() {
        for src in [
            "fn f( {\n    // we use this helper and mod it\n    let s = \"use mod\";\n}\n",
            "struct S {\n    /* we use this */ a: ,\n    b: &'static str = \"mod x\",\n}\n",
            "fn g() {\n    let x = 1 +;\n    // use the force\n    let msg = \"mod me\";\n}\n",
        ] {
            let got = specs("rust", src);
            assert!(!has_parse_error(&got), "{src:?}: {got:?}");
        }
        // A real broken `use` is still reported.
        let got = specs("rust", "use ;\nfn f() {}\n");
        assert_eq!(got, vec![(1, Spec::ParseError("use".into()))]);
    }

    #[test]
    fn csharp_using_statements_in_top_level_errors_are_not_imports() {
        for src in [
            "using System;\nusing var x = Open();\nint y = ;\nConsole.WriteLine(y);\n",
            "using System;\nusing (var s = Open()) { int z = ; }\n",
            "using System;\nawait using var c = Connect();\nvar q = ;\n",
            "Console.WriteLine(1 +);\nusing var x = Open();\n",
            // An unclosed class: the ERROR spans the method body and its statements.
            "using System;\nclass C { void M() { using var x = Open(); int y = ; }\n",
            "using System;\nclass C { void M() { using (var s = Open()) { int y = ; }\n",
            "using System;\nclass C { async void M() { await using var c = Connect(); int y = ; }\n",
            "using System;\nclass C { void M() { using Stream s = Open(); int y = ; }\n",
        ] {
            let got = specs("csharp", src);
            assert!(!has_parse_error(&got), "{src:?}: {got:?}");
        }
        // A broken directive is still reported.
        let got = specs("csharp", "using System;\nusing Foo.;\nclass C {}\n");
        assert!(has_parse_error(&got), "{got:?}");
    }

    #[test]
    fn unsupported_languages_yield_nothing() {
        assert!(extract("markdown", b"import x").is_empty());
    }
}
