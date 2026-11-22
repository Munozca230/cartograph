//! The precise layer (N3): what an imported SCIP index changed in the symbol layer.
//!
//! A SCIP index (written by scip-typescript, rust-analyzer, scip-java, scip-python…) holds
//! compiler-precise definitions and references. For the files it covers, the indexer
//! replaces the heuristic calls with exact ones and adds the definitions N2 missed. What it
//! did is recorded here, next to [`crate::Symbols`]: which files it covers, which symbols
//! it added, and counts for `overview`.

use serde::{Deserialize, Serialize};

/// Coverage of the imported SCIP indexes. Empty when no index was imported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScipCoverage {
    /// Repo-relative paths of the files whose calls come from SCIP (sorted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Symbols SCIP defined that N2 did not find (`source = scip`), sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<ScipSymbol>,
    /// Counts of the last import; `None` when no index was imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<ScipStats>,
}

impl ScipCoverage {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.added.is_empty() && self.stats.is_none()
    }

    /// Sort and deduplicate, so two runs compare equal.
    pub fn normalize(&mut self) {
        self.files.sort();
        self.files.dedup();
        self.added.sort();
        self.added.dedup();
    }
}

/// A symbol added from a SCIP definition: its id and line (ids are not unique).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScipSymbol {
    pub id: String,
    pub line: u32,
}

/// What one import of SCIP indexes did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScipStats {
    /// One entry per SCIP file read (or that failed to read).
    pub indexes: Vec<ScipIndexStats>,
    /// Files whose calls come from SCIP.
    pub files_covered: u64,
    /// SCIP definitions that matched an N2 symbol.
    pub symbols_matched: u64,
    /// SCIP definitions added as symbols (no N2 match).
    pub symbols_added: u64,
    /// Reference occurrences read in covered files (global symbols only).
    pub references: u64,
    /// N2 calls SCIP confirmed (same line and target), now exact.
    pub calls_confirmed: u64,
    /// Of those, the ones N2 had as resolved or heuristic.
    pub calls_upgraded: u64,
    /// Calls SCIP found that N2 did not.
    pub calls_added: u64,
    /// N2 calls in covered files SCIP did not confirm, dropped.
    pub calls_dropped: u64,
    /// File → file dependencies only SCIP saw (`uses` edges).
    pub deps_added: u64,
    /// File → library dependencies only SCIP saw.
    pub externals_added: u64,
    /// External packages referenced that map to a library of the graph.
    pub packages_matched: u64,
    /// External packages referenced with no library of that name in the graph.
    pub packages_unmatched: u64,
}

/// One SCIP file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScipIndexStats {
    /// The file as given (repo-relative when inside the repo).
    pub path: String,
    /// The tool that wrote it (`scip-typescript 0.3.x`).
    pub tool: String,
    /// Folder of the repo its document paths are relative to (`""` = the repo root).
    pub prefix: String,
    pub documents: u64,
    /// Documents whose path is an indexed file.
    pub matched: u64,
    /// Documents whose path is not an indexed file.
    pub unmatched: u64,
    /// Matched documents whose file changed after the index was written: left to N2.
    pub stale: u64,
    pub occurrences: u64,
    /// Read from the cache (the file did not change since the last run).
    pub cached: bool,
    /// Why the file could not be read; nothing of it was used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A few unmatched document paths, for diagnosis.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched_sample: Vec<String>,
}
