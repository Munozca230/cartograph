//! Godot: references out of GDScript (tree-sitter, `imports/queries/gdscript.scm`) and out
//! of Godot's text formats (`.tscn`, `.tres`, `project.godot`, with the small parser in
//! [`text`]).
//!
//! Extraction yields [`Ref`]s ([`refs`]) wrapped in [`crate::spec::Spec::Godot`], so they
//! ride in the extraction cache like any other import. Some refs are declarations, not
//! imports (`class_name`, a scene's own `uid`, a scene's root script, signal connections):
//! the resolver ([`crate::resolve::godot`]) reads them to build its indexes and never
//! turns them into import sites or unresolved records.

pub mod gdscript;
pub mod refs;
pub mod scene;
pub mod text;

pub use refs::{PathKind, Ref, Signal, SignalTarget};
use std::collections::HashSet;
use std::sync::OnceLock;

/// Languages (see [`crate::lang::detect`]) handled here.
pub fn supports(lang: &str) -> bool {
    matches!(
        lang,
        "gdscript" | "godot-scene" | "godot-resource" | "godot-project"
    )
}

/// Bump when extraction changes what it returns for the same input (part of the
/// extraction-cache key, see [`extractor_tag`]).
pub const REVISION: u32 = 2;

/// Part of [`crate::imports::extractor_version`]: revision, a hash of the GDScript query and
/// of the built-in class list (extraction drops built-in names), and the grammar ABI.
pub fn extractor_tag() -> String {
    let mut h = blake3::Hasher::new();
    h.update(gdscript::QUERY.as_bytes());
    h.update(&[0]);
    h.update(BUILTIN_CLASSES.as_bytes());
    let hash = h.finalize().to_hex();
    format!(
        "gd{REVISION}-{}-abi{}",
        &hash.as_str()[..12],
        crate::grammars::Grammar::GdScript.abi_version()
    )
}

/// Extract the references of a Godot file. `None` for languages handled elsewhere.
pub fn extract(lang: &str, source: &[u8]) -> Option<Vec<crate::spec::RawImport>> {
    match lang {
        "gdscript" => Some(gdscript::extract(source)),
        "godot-scene" | "godot-resource" | "godot-project" => {
            Some(scene::extract(lang, &String::from_utf8_lossy(source)))
        }
        _ => None,
    }
}

/// Godot built-in class names (classes, builtin types, singletons) of the engine version
/// named in `builtin_classes.txt`.
pub fn is_builtin_class(name: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        BUILTIN_CLASSES
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    })
    .contains(name)
}

const BUILTIN_CLASSES: &str = include_str!("builtin_classes.txt");

/// Engine version of the built-in class list, for messages.
pub const BUILTIN_VERSION: &str = "4.7";

/// Whether a string names a Godot path worth a reference when it appears as a bare
/// string literal.
pub fn looks_like_resource_path(s: &str) -> bool {
    s.starts_with("res://") || s.starts_with("uid://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_list_is_loaded() {
        for n in [
            "Node",
            "Node2D",
            "Resource",
            "RefCounted",
            "Vector3",
            "Input",
        ] {
            assert!(is_builtin_class(n), "{n}");
        }
        assert!(!is_builtin_class("Player"));
        assert!(!is_builtin_class("# Godot built-in class"));
    }
}
