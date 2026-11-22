//! Tree-sitter grammars bundled with cartograph (see docs/spike-grammars.md).
//!
//! Every grammar crate links against the same `tree-sitter` core through the
//! `tree-sitter-language` shim, so they can coexist in one binary. See
//! `docs/spike-grammars.md` for versions, ABI notes and build requirements.

use tree_sitter::{Language, Node, Parser, Tree};

/// Newest grammar ABI the linked tree-sitter core accepts.
pub const ABI: usize = tree_sitter::LANGUAGE_VERSION;
/// Oldest grammar ABI the linked tree-sitter core accepts.
pub const MIN_COMPATIBLE_ABI: usize = tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION;

/// Languages for which a tree-sitter grammar is compiled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Grammar {
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Rust,
    CSharp,
    Go,
    Java,
    C,
    Cpp,
    GdScript,
}

impl Grammar {
    /// All grammars, in a stable order.
    pub const ALL: [Grammar; 11] = [
        Grammar::TypeScript,
        Grammar::Tsx,
        Grammar::JavaScript,
        Grammar::Python,
        Grammar::Rust,
        Grammar::CSharp,
        Grammar::Go,
        Grammar::Java,
        Grammar::C,
        Grammar::Cpp,
        Grammar::GdScript,
    ];

    /// Short identifier, matching the `lang` strings produced by language detection.
    pub fn name(self) -> &'static str {
        match self {
            Grammar::TypeScript => "typescript",
            Grammar::Tsx => "tsx",
            Grammar::JavaScript => "javascript",
            Grammar::Python => "python",
            Grammar::Rust => "rust",
            Grammar::CSharp => "csharp",
            Grammar::Go => "go",
            Grammar::Java => "java",
            Grammar::C => "c",
            Grammar::Cpp => "cpp",
            Grammar::GdScript => "gdscript",
        }
    }

    /// Grammar for a detected language name (see [`crate::lang::detect`]).
    /// `jsx` uses the JavaScript grammar, which supports JSX.
    pub fn for_lang(lang: &str) -> Option<Grammar> {
        Some(match lang {
            "typescript" => Grammar::TypeScript,
            "tsx" => Grammar::Tsx,
            "javascript" | "jsx" => Grammar::JavaScript,
            "python" => Grammar::Python,
            "rust" => Grammar::Rust,
            "csharp" => Grammar::CSharp,
            "go" => Grammar::Go,
            "java" => Grammar::Java,
            "c" => Grammar::C,
            "cpp" => Grammar::Cpp,
            "gdscript" => Grammar::GdScript,
            _ => return None,
        })
    }

    /// The tree-sitter language object.
    pub fn language(self) -> Language {
        match self {
            Grammar::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Grammar::Python => tree_sitter_python::LANGUAGE.into(),
            Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
            Grammar::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Grammar::Go => tree_sitter_go::LANGUAGE.into(),
            Grammar::Java => tree_sitter_java::LANGUAGE.into(),
            Grammar::C => tree_sitter_c::LANGUAGE.into(),
            Grammar::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Grammar::GdScript => tree_sitter_gdscript::LANGUAGE.into(),
        }
    }

    /// ABI version the grammar was generated with.
    pub fn abi_version(self) -> usize {
        self.language().abi_version()
    }

    /// A parser configured for this grammar.
    pub fn parser(self) -> Result<Parser, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&self.language())?;
        Ok(parser)
    }
}

/// Error from [`parse`].
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("grammar {0} is incompatible with the tree-sitter core: {1}")]
    Language(&'static str, tree_sitter::LanguageError),
    #[error("tree-sitter returned no tree for {0}")]
    NoTree(&'static str),
}

/// Parse `source` with `grammar`.
pub fn parse(grammar: Grammar, source: &[u8]) -> Result<Tree, ParseError> {
    let mut parser = grammar
        .parser()
        .map_err(|e| ParseError::Language(grammar.name(), e))?;
    parser
        .parse(source, None)
        .ok_or(ParseError::NoTree(grammar.name()))
}

/// A syntax problem found in a tree: an ERROR node or a MISSING node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxProblem {
    pub kind: String,
    pub missing: bool,
    pub row: usize,
    pub column: usize,
}

/// Collect every ERROR and MISSING node in the tree.
///
/// Iterative (a `TreeCursor` walk), so arbitrarily deep trees cannot overflow the stack.
pub fn syntax_problems(tree: &Tree) -> Vec<SyntaxProblem> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    loop {
        record(cursor.node(), &mut out);
        if cursor.goto_first_child() {
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

fn record(node: Node<'_>, out: &mut Vec<SyntaxProblem>) {
    if node.is_error() || node.is_missing() {
        let pos = node.start_position();
        out.push(SyntaxProblem {
            kind: node.kind().to_string(),
            missing: node.is_missing(),
            row: pos.row,
            column: pos.column,
        });
    }
}
