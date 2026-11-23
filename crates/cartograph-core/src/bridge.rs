//! Bridges: links between files that no import states. An Electron renderer reaches a
//! main-process handler over an IPC channel, a client reaches a server route over HTTP, a
//! member of a workspace uses another member's package through its manifest.
//!
//! The indexer records both sides of each link ([`BridgeEndpoint`]: what a file provides or
//! consumes, under a key such as the channel `users:get` or the coordinate
//! `maven:com.acme:lib`) and the links it matched ([`Bridge`]). Matched HTTP and IPC links
//! also become derived file → file edges of kind [`crate::EdgeKind::Bridge`]; a package
//! link stays a row (the imports it explains are ordinary import edges).
//!
//! Which bridges are built is set by `[bridges]` ([`BridgeSettings`]): every kind is on in
//! a workspace; in a single repo IPC is on (Electron apps are one repo; extraction is
//! gated on importing `electron` anyway) and packages and HTTP are opt-in.

use crate::Confidence;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// What carries a bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeKind {
    Http,
    Ipc,
    Package,
}

/// Which side of a bridge an endpoint is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeRole {
    /// Serves the key: an IPC handler, a route, a package, a push listener.
    Provide,
    /// Uses the key: an IPC invoke or send, a client call, a dependency, a push sender.
    Consume,
}

macro_rules! bridge_enum {
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

bridge_enum!(BridgeKind { Http => "http", Ipc => "ipc", Package => "package" });
bridge_enum!(BridgeRole { Provide => "provide", Consume => "consume" });

/// One side of a bridge as found in a file, matched or not.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BridgeEndpoint {
    pub kind: BridgeKind,
    pub role: BridgeRole,
    /// What matches the two sides: an IPC channel (`users:get`), a package coordinate
    /// (`maven:com.acme:lib`). Empty when it could not be worked out (a dynamic channel).
    pub key: String,
    /// What the site is, for people: `ipcMain.handle`, `invoke`, `api.getUser`, a version.
    pub label: String,
    /// Path of the file (a manifest for a package).
    pub file: String,
    /// 1-based line (0 for a whole manifest).
    pub line: u32,
    /// The symbol that runs or makes the call, when known (`sym:…`).
    pub symbol: Option<String>,
    /// How the key was found: `exact` (a literal), `resolved` (through constants, a
    /// wrapper or an alias), `heuristic` (a guess).
    pub confidence: Confidence,
    /// Why it matters: `unmatched`, `dynamic`, `unexposed`, a version… `None` when matched.
    pub detail: Option<String>,
}

/// A matched bridge: `src` reaches `dst` over `key`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Bridge {
    pub kind: BridgeKind,
    /// How: `expose` (renderer → preload exposure), `invoke` / `send` (preload → main
    /// handler), `push` (main → renderer listener), `maven` / `npm` (a dependency on a
    /// member's package).
    pub subtype: String,
    pub src_file: String,
    pub src_line: u32,
    /// The symbol at the source, when known.
    pub src: Option<String>,
    pub dst_file: String,
    pub dst_line: u32,
    /// The symbol at the destination (a handler, an exposed function), when known.
    pub dst: Option<String>,
    pub key: String,
    pub confidence: Confidence,
}

/// `[bridges]` as written (in `cartograph-workspace.toml`, or `cartograph.toml` for a single
/// repo): each kind on, off, or left to its default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeSettings {
    pub http: Option<bool>,
    pub ipc: Option<bool>,
    pub packages: Option<bool>,
}

/// Which bridges an index builds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bridges {
    pub http: bool,
    pub ipc: bool,
    pub packages: bool,
}

impl Bridges {
    /// Every kind off.
    pub const OFF: Bridges = Bridges {
        http: false,
        ipc: false,
        packages: false,
    };

    pub fn any(&self) -> bool {
        self.http || self.ipc || self.packages
    }
}

impl BridgeSettings {
    /// Read a `[bridges]` table: `http`, `ipc` and `packages`, each a boolean.
    pub fn parse(v: &toml::Value) -> Result<BridgeSettings, String> {
        let t = v
            .as_table()
            .ok_or_else(|| "[bridges] must be a table".to_string())?;
        let mut s = BridgeSettings::default();
        for (k, v) in t {
            let b = v
                .as_bool()
                .ok_or_else(|| format!("bridges.{k} must be true or false"))?;
            match k.as_str() {
                "http" => s.http = Some(b),
                "ipc" => s.ipc = Some(b),
                "packages" => s.packages = Some(b),
                other => {
                    return Err(format!(
                        "unknown key bridges.{other} (known: http, ipc, packages)"
                    ));
                }
            }
        }
        Ok(s)
    }

    /// What is built: as written, else the default for a workspace (every kind) or a single
    /// repo (IPC only).
    pub fn resolve(&self, workspace: bool) -> Bridges {
        Bridges {
            http: self.http.unwrap_or(workspace),
            ipc: self.ipc.unwrap_or(true),
            packages: self.packages.unwrap_or(workspace),
        }
    }

    /// `[bridges]` of `<root>/cartograph.toml` (a single repo). A missing file or table
    /// gives the defaults; so does one that does not parse (the rules reader reports it).
    pub fn of_repo(root: &Path) -> BridgeSettings {
        let Ok(text) = std::fs::read_to_string(root.join("cartograph.toml")) else {
            return BridgeSettings::default();
        };
        let Ok(t) = text.parse::<toml::Table>() else {
            return BridgeSettings::default();
        };
        match t.get("bridges") {
            Some(v) => BridgeSettings::parse(v).unwrap_or_default(),
            None => BridgeSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_per_mode_and_override() {
        let none = BridgeSettings::default();
        assert_eq!(
            none.resolve(true),
            Bridges {
                http: true,
                ipc: true,
                packages: true
            }
        );
        assert_eq!(
            none.resolve(false),
            Bridges {
                http: false,
                ipc: true,
                packages: false
            }
        );
        let v: toml::Value = toml::from_str("ipc = false\npackages = true").unwrap();
        let s = BridgeSettings::parse(&v).unwrap();
        assert_eq!(
            s.resolve(false),
            Bridges {
                http: false,
                ipc: false,
                packages: true
            }
        );
        let bad: toml::Value = toml::from_str("ipcs = true").unwrap();
        assert!(
            BridgeSettings::parse(&bad)
                .unwrap_err()
                .contains("bridges.ipcs")
        );
        let bad: toml::Value = toml::from_str("ipc = 1").unwrap();
        assert!(BridgeSettings::parse(&bad).is_err());
    }

    #[test]
    fn kinds_round_trip() {
        for k in [BridgeKind::Http, BridgeKind::Ipc, BridgeKind::Package] {
            assert_eq!(k.as_str().parse::<BridgeKind>().unwrap(), k);
        }
        for r in [BridgeRole::Provide, BridgeRole::Consume] {
            assert_eq!(r.as_str().parse::<BridgeRole>().unwrap(), r);
        }
    }
}
