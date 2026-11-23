//! Core types for cartograph: the graph model and its SQLite store.

// Library code must not panic or write to stdout/stderr (logs go through `tracing`).
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

pub mod bridge;
pub mod history;
pub mod key;
pub mod manifest_entry;
pub mod model;
pub mod scip;
pub mod spring;
pub mod store;
pub mod workspace;

pub use bridge::{Bridge, BridgeEndpoint, BridgeKind, BridgeRole, BridgeSettings, Bridges};
pub use history::{
    AuthorShare, ChangeKind, CoChange, FileHistory, GitAuthor, GitChange, GitCommit, GitRaw,
    GitState, GitStatus, History,
};
pub use key::{KeyRole, KeySite};
pub use model::{
    CallStats, Confidence, Diagnostic, Edge, EdgeKind, EntryKind, EntryPoint, Graph, ImportSite,
    Node, NodeKind, SkipReason, SkippedFile, SuperType, Symbol, SymbolCall, SymbolKind, Symbols,
    UnresolvedImport, UnresolvedReason, merge_edges, merge_skipped, node_id, split_path_name,
    split_symbol_id, symbol_id,
};
pub use scip::{ScipCoverage, ScipIndexStats, ScipStats, ScipSymbol};
pub use spring::{DataSink, InjectedUse, ResourceName, SpringBean, SpringInjection};
pub use store::{
    BUSY_TIMEOUT, CacheEntry, GIT_META, SCHEMA_VERSION, ScipCacheRow, Store, WriteTx, git_meta_key,
};
pub use workspace::{Member, MemberHttp, WORKSPACE_FILE, Workspace, WorkspaceError, expand_alias};

/// Errors from the core crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown {ty} value {value:?} in the database")]
    UnknownVariant { ty: &'static str, value: String },
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("could not enable WAL journal mode (got {0:?})")]
    NotWal(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
