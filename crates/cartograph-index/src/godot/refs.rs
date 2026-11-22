//! What Godot extraction finds: the [`Ref`]s that [`crate::spec::Spec::Godot`] carries.
//! Plain data with no dependency, so the import vocabulary ([`crate::spec`]) can name it
//! without depending on the parsers.

use serde::{Deserialize, Serialize};

/// One reference or declaration found in a Godot file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ref {
    /// A path: `res://…`, `uid://…`, a path relative to the file (`load("enemy.gd")`) or
    /// `user://…`. `role` says how it is used (`preload`, `load`, `extends`, `string`,
    /// `script`, `instance`, `inherits`, `main_scene`, `autoload Name`, an ext_resource type
    /// such as `Texture2D`, or a property name); the import site's specifier is
    /// `"{role} {path}"`.
    Path {
        role: String,
        path: String,
        /// The `uid="uid://…"` an `ext_resource` carries next to its path.
        uid: Option<String>,
        kind: PathKind,
    },
    /// A global name used in code: a `class_name` or an autoload. `extends` for `extends
    /// Name`; other uses are emitted only for names that are not Godot built-ins nor
    /// declared in the file.
    Name { name: String, extends: bool },
    /// A path computed at run time (`load(path)`, `"res://levels/%s.tscn" % n`).
    Dynamic { role: String, text: String },
    /// `class_name Name` (declaration).
    DeclareClass(String),
    /// The scene's or resource's own `uid="uid://…"` (declaration).
    DeclareUid(String),
    /// The script attached to a scene's root node (declaration; for signal targets that
    /// are instances of this scene).
    RootScript { path: String, uid: Option<String> },
    /// `project.godot` `[autoload] Name="*res://…"`: an import of the target and the
    /// declaration of a global name.
    Autoload { name: String, path: String },
    /// A `[connection]` of a scene (data; it becomes a `Call` edge to the handler's script
    /// when that script is known).
    Signal(Signal),
}

/// How sure a path reference is, and whether it is an inheritance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathKind {
    /// Structural: `preload`, `load`, `ext_resource`, `main_scene`, `autoload`.
    Direct,
    /// `extends "res://…"`, or an inherited scene's root `instance`.
    Extends,
    /// A string literal that names a path (`Heuristic`).
    String,
}

/// A `[connection signal=… from=… to=… method=…]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    pub signal: String,
    pub from: String,
    pub to: String,
    pub method: String,
    pub target: SignalTarget,
}

/// What the `to` node of a connection is, as far as its own scene says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalTarget {
    /// The node has this script attached.
    Script { path: String, uid: Option<String> },
    /// The node is an instance of this scene; the handler is that scene's root script.
    Instance { path: String, uid: Option<String> },
    /// The node exists and has no script.
    NoScript,
    /// No node with that path in this scene (it may live inside an instanced or inherited
    /// scene).
    UnknownNode,
}
