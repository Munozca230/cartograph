//! The kinds and severities of findings: plain names, so the config ([`crate::config`])
//! can name them without depending on the analyses. Re-exported from [`super`].

use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A rule of `cartograph.toml` on layers.
    LayerViolation,
    /// A file depends on more files than `rules.max_fan_out`.
    FanOutLimit,
    /// Files that depend on each other in a loop.
    Cycle,
    /// Sibling folders that depend on each other in a loop.
    FolderCycle,
    /// Big, depended upon and dependent at once.
    GodModule,
    /// On an unusual share of all dependency paths (betweenness).
    Hub,
    /// A folder depends on a less stable one (Martin's instability).
    Sdp,
    /// Nothing depends on it and it is not an entry point.
    Orphan,
    /// An unusually long chain of dependencies.
    DeepChain,
    /// Two files that change together in git but have no dependency path either way.
    HiddenCoupling,
    /// An IPC channel used (`invoke`, `send`) that no main-process handler serves.
    IpcNoHandler,
    /// A renderer calls a function of an exposed world the preload does not expose.
    IpcUnexposed,
    /// An IPC handler no `invoke` or `send` of the repo uses.
    IpcUnusedHandler,
    /// A member imports another member's code without declaring its package.
    UndeclaredMemberDependency,
    /// A member depends on another member's package at another version than it has.
    VersionSkew,
    /// An HTTP call whose server is known (a member, a declared host, a base whose other
    /// calls reach routes) that no route serves.
    HttpUnmatchedCall,
    /// An HTTP call to a route that exists with another method.
    HttpMethodMismatch,
    /// A route no member calls, in a member other members do call.
    HttpUnusedRoute,
}

impl FindingKind {
    pub const ALL: [FindingKind; 18] = [
        FindingKind::LayerViolation,
        FindingKind::FanOutLimit,
        FindingKind::Cycle,
        FindingKind::FolderCycle,
        FindingKind::GodModule,
        FindingKind::Hub,
        FindingKind::Sdp,
        FindingKind::Orphan,
        FindingKind::DeepChain,
        FindingKind::HiddenCoupling,
        FindingKind::IpcNoHandler,
        FindingKind::IpcUnexposed,
        FindingKind::IpcUnusedHandler,
        FindingKind::UndeclaredMemberDependency,
        FindingKind::VersionSkew,
        FindingKind::HttpUnmatchedCall,
        FindingKind::HttpMethodMismatch,
        FindingKind::HttpUnusedRoute,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FindingKind::LayerViolation => "layer_violation",
            FindingKind::FanOutLimit => "fan_out_limit",
            FindingKind::Cycle => "cycle",
            FindingKind::FolderCycle => "folder_cycle",
            FindingKind::GodModule => "god_module",
            FindingKind::Hub => "hub",
            FindingKind::Sdp => "sdp",
            FindingKind::Orphan => "orphan",
            FindingKind::DeepChain => "deep_chain",
            FindingKind::HiddenCoupling => "hidden_coupling",
            FindingKind::IpcNoHandler => "ipc_no_handler",
            FindingKind::IpcUnexposed => "ipc_unexposed",
            FindingKind::IpcUnusedHandler => "ipc_unused_handler",
            FindingKind::UndeclaredMemberDependency => "undeclared_member_dependency",
            FindingKind::VersionSkew => "version_skew",
            FindingKind::HttpUnmatchedCall => "http_unmatched_call",
            FindingKind::HttpMethodMismatch => "http_method_mismatch",
            FindingKind::HttpUnusedRoute => "http_unused_route",
        }
    }
}

impl fmt::Display for FindingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for FindingKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm = s.trim().to_ascii_lowercase().replace('-', "_");
        let alias = match norm.as_str() {
            "layer" | "layers" => "layer_violation",
            "fan_out" | "fanout" => "fan_out_limit",
            "cycles" => "cycle",
            "folder_cycles" => "folder_cycle",
            "god" | "god_modules" => "god_module",
            "hubs" | "bottleneck" | "bottlenecks" => "hub",
            "stable_dependencies" | "instability" => "sdp",
            "orphans" | "dead" => "orphan",
            "chain" | "chains" | "deep_chains" => "deep_chain",
            "hidden" | "coupling" | "co_change" | "cochange" | "temporal_coupling" => {
                "hidden_coupling"
            }
            other => other,
        };
        FindingKind::ALL
            .into_iter()
            .find(|k| k.as_str() == alias)
            .ok_or_else(|| format!("unknown finding kind {s:?}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

impl std::str::FromStr for Severity {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" | "errors" => Ok(Severity::Error),
            "warning" | "warnings" | "warn" => Ok(Severity::Warning),
            "info" => Ok(Severity::Info),
            other => Err(format!(
                "severity must be error, warning or info, got {other:?}"
            )),
        }
    }
}
