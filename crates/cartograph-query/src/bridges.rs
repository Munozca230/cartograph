//! Bridges as queries see them (see [`cartograph_core::bridge`]): the endpoints and the
//! matched bridges of the symbol layer, indexed by symbol and by file.

use cartograph_core::{Bridge, BridgeEndpoint, BridgeKind, Symbols};
use std::collections::HashMap;

/// Endpoint details the indexer writes (`cartograph_index::symbols::resolve::bridge`).
pub const UNMATCHED: &str = "unmatched";
pub const UNUSED: &str = "unused";
pub const UNEXPOSED: &str = "unexposed";
pub const DYNAMIC: &str = "dynamic";

/// The bridges of an index, indexed.
#[derive(Debug, Default)]
pub struct BridgeTable {
    pub endpoints: Vec<BridgeEndpoint>,
    pub bridges: Vec<Bridge>,
    /// Destination symbol → bridges.
    into_sym: HashMap<String, Vec<usize>>,
    /// Source file → bridges.
    from_file: HashMap<String, Vec<usize>>,
    /// Destination file → bridges.
    into_file: HashMap<String, Vec<usize>>,
    /// File → endpoints.
    endpoints_of: HashMap<String, Vec<usize>>,
}

impl BridgeTable {
    pub fn build(s: &Symbols) -> BridgeTable {
        let mut t = BridgeTable {
            endpoints: s.endpoints.clone(),
            bridges: s.bridges.clone(),
            ..BridgeTable::default()
        };
        for (i, b) in t.bridges.iter().enumerate() {
            if let Some(d) = &b.dst {
                t.into_sym.entry(d.clone()).or_default().push(i);
            }
            t.from_file.entry(b.src_file.clone()).or_default().push(i);
            t.into_file.entry(b.dst_file.clone()).or_default().push(i);
        }
        for (i, e) in t.endpoints.iter().enumerate() {
            t.endpoints_of.entry(e.file.clone()).or_default().push(i);
        }
        t
    }

    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty() && self.bridges.is_empty()
    }

    /// Bridges that reach symbol `id`.
    pub fn into_symbol(&self, id: &str) -> impl Iterator<Item = &Bridge> {
        self.into_sym
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.bridges[i])
    }

    /// Bridges that leave file `path`.
    pub fn from_file(&self, path: &str) -> impl Iterator<Item = &Bridge> {
        self.from_file
            .get(path)
            .into_iter()
            .flatten()
            .map(|&i| &self.bridges[i])
    }

    /// Bridges that reach file `path`.
    pub fn into_file(&self, path: &str) -> impl Iterator<Item = &Bridge> {
        self.into_file
            .get(path)
            .into_iter()
            .flatten()
            .map(|&i| &self.bridges[i])
    }

    /// Endpoints found in file `path`.
    pub fn endpoints_of(&self, path: &str) -> impl Iterator<Item = &BridgeEndpoint> {
        self.endpoints_of
            .get(path)
            .into_iter()
            .flatten()
            .map(|&i| &self.endpoints[i])
    }

    /// The bridges from `src` to `dst` (files).
    pub fn between(&self, src: &str, dst: &str) -> impl Iterator<Item = &Bridge> {
        self.from_file(src).filter(move |b| b.dst_file == dst)
    }
}

/// `ipc`, `http` or `package`, as people read it: `IPC`, `HTTP`, `package`.
pub fn kind_label(k: BridgeKind) -> &'static str {
    match k {
        BridgeKind::Http => "HTTP",
        BridgeKind::Ipc => "IPC",
        BridgeKind::Package => "package",
    }
}

/// One step over a bridge, for paths and dependency lists: `ipc invoke users:get`.
pub fn step_label(b: &Bridge) -> String {
    format!("{} {} {}", b.kind, b.subtype, b.key)
}
