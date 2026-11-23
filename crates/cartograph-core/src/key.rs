//! String keys: where a key such as an action name (`"READ-FILE"`) or a step name is
//! defined, run, listed or used, across the files (and members) of an index. `find` traces
//! a key from these sites, and says where a key is missing from a container that holds
//! its siblings.
//!
//! A site's [`KeyRole`] comes from its form: an enum constant or a string constant
//! *defines* the key; a switch case, a registration that binds it to code or an annotation
//! that names a class *provides* (runs) it; an entry of a catalog (an array, a map keyed
//! by strings, `List.of`) *lists* it; anything else *uses* it. Sites belong to the symbol
//! layer and are cleared with it.

use crate::Confidence;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// What a key site does with the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyRole {
    /// An enum constant or a string constant.
    Define,
    /// A switch case, a registration binding it to code, a naming annotation.
    Provide,
    /// An entry of a catalog (an array, a map keyed by strings, a list built of keys).
    List,
    /// Any other reference.
    Use,
}

impl KeyRole {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyRole::Define => "define",
            KeyRole::Provide => "provide",
            KeyRole::List => "list",
            KeyRole::Use => "use",
        }
    }
}

impl fmt::Display for KeyRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for KeyRole {
    type Err = crate::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "define" => KeyRole::Define,
            "provide" => KeyRole::Provide,
            "list" => KeyRole::List,
            "use" => KeyRole::Use,
            other => {
                return Err(crate::Error::UnknownVariant {
                    ty: "KeyRole",
                    value: other.to_string(),
                });
            }
        })
    }
}

/// One site of a key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KeySite {
    /// The key, as its value reads (`READ-FILE`), constants read through.
    pub key: String,
    pub role: KeyRole,
    /// How it is written: `enum`, `const`, `case`, `register`, `annotation`, `entry`,
    /// `use`.
    pub form: String,
    /// The constant that defines it (`Action.READ_FILE`), for a definition.
    pub name: Option<String>,
    /// What holds it, for people: the enum, the class of constants (`Steps.STEP_*`), the
    /// switch, the registering class and callee, the catalog's declaration and path, the
    /// annotation.
    pub container: String,
    /// The file of the container (`""` when it spans files, as an annotation's).
    pub container_file: String,
    /// First line of the container (0 when not known).
    pub container_line: u32,
    pub file: String,
    pub line: u32,
    /// The function, method or class around the site (`sym:…`).
    pub symbol: Option<String>,
    /// The code a case runs or a registration binds the key to (`sym:…`), when resolved.
    pub target: Option<String>,
    /// `exact` (a literal), `resolved` (through a constant), `heuristic` (a bare name
    /// matched by its last part).
    pub confidence: Confidence,
}

impl KeySite {
    /// Sites of one container share this: `(container_file, container_line, container)`.
    pub fn container_key(&self) -> (&str, u32, &str) {
        (&self.container_file, self.container_line, &self.container)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip() {
        for r in [
            KeyRole::Define,
            KeyRole::Provide,
            KeyRole::List,
            KeyRole::Use,
        ] {
            assert_eq!(r.as_str().parse::<KeyRole>().unwrap(), r);
        }
        assert!("x".parse::<KeyRole>().is_err());
    }
}
