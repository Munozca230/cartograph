//! Read-only queries and findings over an indexed cartograph graph.
//!
//! Every query is a pure function of a [`Repo`] (the graph plus `cartograph.toml`) that
//! returns an [`Answer`]: a `Serialize` struct and a compact text rendering, both within the
//! query's token budget ([`budget::limits`]). Ids are stable (`file:<path>`, `dir:<path>`,
//! `repo:`, stored external ids such as `external:npm:react`); inputs also accept bare paths.
//! An unknown id is an error with the closest matches. Nothing here panics or prints.
//!
//! The CLI exposes these as `cartograph query <tool>`, the server feeds findings to the
//! web map, and the MCP server wraps the same functions.

#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr
    )
)]

pub mod algo;
pub mod bridges;
pub mod budget;
pub mod check;
pub mod config;
pub mod deps;
pub mod diff;
pub mod dispatch;
pub mod explain;
pub mod find;
pub mod findings;
pub mod flow;
pub mod graph;
pub mod hierarchy;
pub mod history;
pub mod hotspots;
pub mod impact;
pub mod infer;
pub mod keys;
pub mod kind;
pub mod list;
pub mod overview;
pub mod path;
pub mod runnable;
pub mod slice;
pub mod spring;
pub mod symbols;
mod test_trees;
mod text;
pub mod workspace;

pub use budget::Answer;
pub use config::{Config, ConfigError};
pub use findings::{Finding, FindingKind, Severity};
pub use graph::{NodeRef, RepoGraph};

use cartograph_core::{Graph, History, Symbols};
use std::sync::OnceLock;

/// Errors a query returns as data (never a panic).
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum QueryError {
    #[error("unknown node {input:?}{}", suggest(.suggestions))]
    UnknownNode {
        input: String,
        suggestions: Vec<String>,
    },
    #[error("{0}")]
    BadArgument(String),
    /// A symbol name that several definitions share.
    #[error("{input:?} names {count} symbols; pass one id: {}", .candidates.join(", "))]
    Ambiguous {
        input: String,
        count: usize,
        candidates: Vec<String>,
    },
    /// A path relative to a member that several members of the workspace have.
    #[error("{input:?} is in {} members; pass one: {}", .candidates.len(), .candidates.join(", "))]
    AmbiguousPath {
        input: String,
        candidates: Vec<String>,
    },
}

fn suggest(s: &[String]) -> String {
    if s.is_empty() {
        "; try `cartograph query find <text>`".to_string()
    } else {
        format!("; did you mean: {}?", s.join(", "))
    }
}

/// An indexed repo ready to query: the graph, the config, and findings computed on first
/// use.
pub struct Repo {
    pub graph: RepoGraph,
    pub config: Config,
    /// The symbol layer (N2); empty (and `indexed == false`) when built with [`Repo::new`].
    pub symbols: symbols::SymbolTable,
    /// The git layer; empty when built without [`Repo::with_history`].
    pub git: history::GitTable,
    /// Imports that did not resolve (for `diff`).
    pub unresolved: Vec<cartograph_core::UnresolvedImport>,
    findings: OnceLock<Vec<Finding>>,
}

impl Repo {
    /// A repo without its symbol layer (file-level queries only).
    pub fn new(graph: &Graph, config: Config) -> Repo {
        Repo {
            graph: RepoGraph::build(graph),
            config,
            symbols: symbols::SymbolTable::default(),
            git: history::GitTable::default(),
            unresolved: graph.unresolved.clone(),
            findings: OnceLock::new(),
        }
    }

    /// A repo with its symbol layer. Findings never read it: file-level results are the same
    /// as with [`Repo::new`].
    pub fn with_symbols(graph: &Graph, symbols: &Symbols, config: Config) -> Repo {
        let repo = Repo::new(graph, config);
        let mut table = symbols::SymbolTable::build(symbols);
        table.members = repo.graph.members.clone();
        let g = &repo.graph;
        let links = spring::SpringTable::link(&table, |p| {
            g.file_by_path(p)
                .is_some_and(|f| test_trees::in_test_tree(g, f, graph::ROOT))
        });
        table.spring.set_links(links);
        Repo {
            symbols: table,
            ..repo
        }
    }

    /// Add the bridges of an index built without its symbol layer (a git base for `diff`
    /// and `check`), so its findings about bridges and its bridges compare with a full index.
    pub fn with_bridges(
        mut self,
        endpoints: &[cartograph_core::BridgeEndpoint],
        bridges: &[cartograph_core::Bridge],
    ) -> Repo {
        let s = cartograph_core::Symbols {
            endpoints: endpoints.to_vec(),
            bridges: bridges.to_vec(),
            ..cartograph_core::Symbols::default()
        };
        self.symbols.bridges = bridges::BridgeTable::build(&s);
        self.findings = OnceLock::new();
        self
    }

    /// In a workspace, the member plain paths are looked for in first (`--member`).
    pub fn set_default_member(&mut self, member: Option<String>) {
        self.graph.members.default = member.clone();
        self.symbols.members.default = member;
    }

    /// Add the git history (churn, owners, co-change): `explain`, `impact` and `hotspots`
    /// use it, and the hidden-coupling finding needs it.
    pub fn with_history(self, history: &History) -> Repo {
        Repo {
            git: history::GitTable::build(&self.graph, history),
            findings: OnceLock::new(),
            ..self
        }
    }

    /// Every finding, sorted (violations, then severity, kind and id).
    pub fn findings(&self) -> &[Finding] {
        self.findings.get_or_init(|| {
            findings::analyze_with(&self.graph, &self.config, &self.git, &self.symbols.bridges)
        })
    }

    /// Resolve an id or path typed by a user or agent.
    pub fn resolve(&self, input: &str) -> Result<NodeRef, QueryError> {
        find::resolve(&self.graph, input)
    }
}
