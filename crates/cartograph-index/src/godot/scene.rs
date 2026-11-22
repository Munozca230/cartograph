//! References out of `.tscn`, `.tres` and `project.godot` (parsed with [`super::text`]).
//!
//! Scenes and resources:
//! - every `[ext_resource]` is one reference at its line, `role` = `script` when some
//!   `script = ExtResource(id)` uses it, `instance` when a node instances it, `inherits`
//!   when the root node instances it (an inherited scene), else its `type` (`Texture2D`,
//!   `PackedScene`…). It carries both `path` and `uid` when both are written;
//! - `instance_placeholder="res://…"` on a node, and any property whose whole value is a
//!   `res://` / `uid://` string, are string references (role = attribute or property name);
//! - the header's `uid` and the root node's script are declarations;
//! - every `[connection]` is a [`Signal`] with what the scene knows about its `to` node.
//!
//! `project.godot`: `run/main_scene` (role `main_scene`), each `[autoload]` entry, and any
//! other `res://` / `uid://` string in a setting (role = setting key).
//!
//! An `ExtResource(id)` with no matching `[ext_resource]` and every parse error become
//! `ParseError` records, so nothing is dropped.

use super::text::{self, Doc, Section};
use super::{PathKind, Ref, Signal, SignalTarget, looks_like_resource_path};
use crate::spec::{RawImport, Spec};
use std::collections::{BTreeMap, HashMap};

fn push(out: &mut Vec<RawImport>, line: u32, r: Ref) {
    out.push(RawImport {
        line,
        spec: Spec::Godot(r),
    });
}

fn parse_error(out: &mut Vec<RawImport>, line: u32, msg: String) {
    out.push(RawImport {
        line,
        spec: Spec::ParseError(msg),
    });
}

/// Extract the references of a scene, resource or project file.
pub fn extract(lang: &str, source: &str) -> Vec<RawImport> {
    let doc = text::parse(source);
    let mut out = Vec::new();
    for (line, msg) in &doc.errors {
        parse_error(&mut out, *line, msg.clone());
    }
    if lang == "godot-project" {
        project(&doc, &mut out);
    } else {
        scene(&doc, &mut out);
    }
    out.sort_by_key(|r| r.line);
    out
}

struct ExtRes<'d> {
    section: &'d Section,
    path: Option<String>,
    uid: Option<String>,
    kind: String,
}

impl ExtRes<'_> {
    /// Path for a reference: the path, else the uid.
    fn reference(&self) -> Option<(String, Option<String>)> {
        match (&self.path, &self.uid) {
            (Some(p), uid) => Some((p.clone(), uid.clone())),
            (None, Some(u)) => Some((u.clone(), None)),
            (None, None) => None,
        }
    }
}

#[derive(Default)]
struct NodeInfo {
    script: Option<String>,
    instance: Option<String>,
}

fn scene(doc: &Doc, out: &mut Vec<RawImport>) {
    // The header's own uid.
    if let Some(head) = doc
        .sections
        .first()
        .filter(|s| s.tag == "gd_scene" || s.tag == "gd_resource")
        && let Some(uid) = head.attr_str("uid").filter(|u| u.starts_with("uid://"))
    {
        push(out, head.line, Ref::DeclareUid(uid));
    }

    let mut ext: BTreeMap<String, ExtRes<'_>> = BTreeMap::new();
    for s in doc.sections.iter().filter(|s| s.tag == "ext_resource") {
        let Some(id) = s.attr_str("id") else {
            parse_error(out, s.line, "[ext_resource] without id".into());
            continue;
        };
        let res = ExtRes {
            section: s,
            path: s.attr_str("path").filter(|p| !p.is_empty()),
            uid: s.attr_str("uid").filter(|u| !u.is_empty()),
            kind: s.attr_str("type").unwrap_or_else(|| "Resource".into()),
        };
        if res.path.is_none() && res.uid.is_none() {
            parse_error(
                out,
                s.line,
                format!("[ext_resource] {id} has no path nor uid"),
            );
        }
        ext.insert(id, res);
    }

    // How each ext_resource is used; nodes by path for signal targets.
    let mut role: HashMap<&str, &'static str> = HashMap::new();
    let mut nodes: HashMap<String, NodeInfo> = HashMap::new();
    let mut root_seen = false;
    let mut root_script: Option<String> = None;
    let check_id = |id: &str, line: u32, out: &mut Vec<RawImport>| -> bool {
        if ext.contains_key(id) {
            true
        } else {
            parse_error(
                out,
                line,
                format!("ExtResource(\"{id}\") has no [ext_resource]"),
            );
            false
        }
    };
    for s in &doc.sections {
        let is_node = s.tag == "node";
        let mut info = NodeInfo::default();
        if is_node && let Some(v) = s.attr("instance") {
            for id in text::ext_refs(v) {
                if check_id(&id, s.line, out) {
                    let is_root = s.attr("parent").is_none() && !root_seen;
                    let r = if is_root { "inherits" } else { "instance" };
                    if let Some((k, _)) = ext.get_key_value(id.as_str()) {
                        role.insert(k.as_str(), r);
                    }
                    info.instance = Some(id);
                }
            }
        }
        if is_node && let Some(p) = s.attr_str("instance_placeholder") {
            push(
                out,
                s.line,
                Ref::Path {
                    role: "instance_placeholder".into(),
                    path: p,
                    uid: None,
                    kind: PathKind::String,
                },
            );
        }
        for p in &s.props {
            let ids = text::ext_refs(&p.value);
            for id in &ids {
                if check_id(id, p.line, out)
                    && p.key == "script"
                    && let Some((k, _)) = ext.get_key_value(id.as_str())
                {
                    role.entry(k.as_str()).or_insert("script");
                    if is_node {
                        info.script = Some(id.clone());
                    }
                }
            }
            if ids.is_empty()
                && let Some(v) = text::unquote(&p.value)
                && looks_like_resource_path(&v)
            {
                push(
                    out,
                    p.line,
                    Ref::Path {
                        role: p.key.clone(),
                        path: v,
                        uid: None,
                        kind: PathKind::String,
                    },
                );
            }
        }
        if is_node {
            let path = match (s.attr_str("name"), s.attr_str("parent")) {
                (_, None) if !root_seen => {
                    root_seen = true;
                    root_script = info.script.clone();
                    ".".to_string()
                }
                (Some(name), Some(parent)) if parent == "." => name,
                (Some(name), Some(parent)) => format!("{parent}/{name}"),
                (name, _) => {
                    parse_error(
                        out,
                        s.line,
                        format!("[node] {} without parent", name.unwrap_or_default()),
                    );
                    continue;
                }
            };
            nodes.insert(path, info);
        }
    }

    for (id, r) in &ext {
        let Some((path, uid)) = r.reference() else {
            continue;
        };
        let used = role.get(id.as_str()).copied();
        push(
            out,
            r.section.line,
            Ref::Path {
                role: used.map_or_else(|| r.kind.clone(), str::to_string),
                path,
                uid,
                kind: if used == Some("inherits") {
                    PathKind::Extends
                } else {
                    PathKind::Direct
                },
            },
        );
    }
    let target_of = |id: &String| ext.get(id).and_then(ExtRes::reference);
    if let Some((path, uid)) = root_script.as_ref().and_then(target_of)
        && let Some(root) = doc.sections.iter().find(|s| s.tag == "node")
    {
        push(out, root.line, Ref::RootScript { path, uid });
    }

    for s in doc.sections.iter().filter(|s| s.tag == "connection") {
        let get = |k: &str| s.attr_str(k).unwrap_or_default();
        let to = get("to");
        let target = match nodes.get(&to) {
            Some(n) => match (
                n.script.as_ref().and_then(target_of),
                n.instance.as_ref().and_then(target_of),
            ) {
                (Some((path, uid)), _) => SignalTarget::Script { path, uid },
                (None, Some((path, uid))) => SignalTarget::Instance { path, uid },
                (None, None) => SignalTarget::NoScript,
            },
            None => SignalTarget::UnknownNode,
        };
        push(
            out,
            s.line,
            Ref::Signal(Signal {
                signal: get("signal"),
                from: get("from"),
                to,
                method: get("method"),
                target,
            }),
        );
    }
}

fn project(doc: &Doc, out: &mut Vec<RawImport>) {
    let settings = doc.preamble.iter().map(|p| ("", p)).chain(
        doc.sections
            .iter()
            .flat_map(|s| s.props.iter().map(move |p| (s.tag.as_str(), p))),
    );
    for (section, p) in settings {
        match (section, p.key.as_str()) {
            ("application", "run/main_scene") => {
                let path = text::unquote(&p.value).unwrap_or_else(|| p.value.clone());
                push(
                    out,
                    p.line,
                    Ref::Path {
                        role: "main_scene".into(),
                        path,
                        uid: None,
                        kind: PathKind::Direct,
                    },
                );
            }
            ("autoload", name) => {
                let value = text::unquote(&p.value).unwrap_or_else(|| p.value.clone());
                let path = value.strip_prefix('*').unwrap_or(&value).to_string();
                push(
                    out,
                    p.line,
                    Ref::Autoload {
                        name: name.to_string(),
                        path,
                    },
                );
            }
            (_, key) => {
                for s in text::strings_in(&p.value) {
                    if looks_like_resource_path(&s) {
                        push(
                            out,
                            p.line,
                            Ref::Path {
                                role: key.to_string(),
                                path: s,
                                uid: None,
                                kind: PathKind::String,
                            },
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(lang: &str, src: &str) -> Vec<(u32, Spec)> {
        extract(lang, src)
            .into_iter()
            .map(|r| (r.line, r.spec))
            .collect()
    }

    fn path(role: &str, p: &str, uid: Option<&str>, kind: PathKind) -> Spec {
        Spec::Godot(Ref::Path {
            role: role.into(),
            path: p.into(),
            uid: uid.map(str::to_string),
            kind,
        })
    }

    #[test]
    fn scene_references_roles_and_signals() {
        let src = "[gd_scene load_steps=4 format=3 uid=\"uid://main\"]\n\
            [ext_resource type=\"Script\" uid=\"uid://s1\" path=\"res://main.gd\" id=\"1_s\"]\n\
            [ext_resource type=\"PackedScene\" path=\"res://player.tscn\" id=\"2_p\"]\n\
            [ext_resource type=\"Texture2D\" uid=\"uid://tex\" id=\"3_t\"]\n\
            [node name=\"Main\" type=\"Node2D\"]\n\
            script = ExtResource(\"1_s\")\n\
            next = \"res://level2.tscn\"\n\
            [node name=\"Player\" parent=\".\" instance=ExtResource(\"2_p\")]\n\
            [node name=\"Icon\" type=\"Sprite2D\" parent=\".\"]\n\
            texture = ExtResource(\"3_t\")\n\
            [node name=\"Ghost\" type=\"Node\" parent=\"Icon\"]\n\
            [connection signal=\"hit\" from=\"Player\" to=\".\" method=\"_on_hit\"]\n\
            [connection signal=\"a\" from=\".\" to=\"Player\" method=\"_on_a\"]\n\
            [connection signal=\"b\" from=\".\" to=\"Icon/Ghost\" method=\"_on_b\"]\n\
            [connection signal=\"c\" from=\".\" to=\"Player/Gun\" method=\"_on_c\"]\n";
        let sig = |signal: &str, to: &str, method: &str, target: SignalTarget| {
            Spec::Godot(Ref::Signal(Signal {
                signal: signal.into(),
                from: if signal == "hit" { "Player" } else { "." }.into(),
                to: to.into(),
                method: method.into(),
                target,
            }))
        };
        assert_eq!(
            refs("godot-scene", src),
            vec![
                (1, Spec::Godot(Ref::DeclareUid("uid://main".into()))),
                (
                    2,
                    path(
                        "script",
                        "res://main.gd",
                        Some("uid://s1"),
                        PathKind::Direct
                    )
                ),
                (
                    3,
                    path("instance", "res://player.tscn", None, PathKind::Direct)
                ),
                (4, path("Texture2D", "uid://tex", None, PathKind::Direct)),
                (
                    5,
                    Spec::Godot(Ref::RootScript {
                        path: "res://main.gd".into(),
                        uid: Some("uid://s1".into())
                    })
                ),
                (7, path("next", "res://level2.tscn", None, PathKind::String)),
                (
                    12,
                    sig(
                        "hit",
                        ".",
                        "_on_hit",
                        SignalTarget::Script {
                            path: "res://main.gd".into(),
                            uid: Some("uid://s1".into())
                        }
                    )
                ),
                (
                    13,
                    sig(
                        "a",
                        "Player",
                        "_on_a",
                        SignalTarget::Instance {
                            path: "res://player.tscn".into(),
                            uid: None
                        }
                    )
                ),
                (14, sig("b", "Icon/Ghost", "_on_b", SignalTarget::NoScript)),
                (
                    15,
                    sig("c", "Player/Gun", "_on_c", SignalTarget::UnknownNode)
                ),
            ]
        );
    }

    #[test]
    fn inherited_scenes_godot3_ids_and_dangling_ids() {
        let src = "[gd_scene load_steps=2 format=2]\n\
            [ext_resource path=\"res://base.tscn\" type=\"PackedScene\" id=1]\n\
            [node name=\"Boss\" instance=ExtResource( 1 )]\n\
            [node name=\"X\" parent=\".\"]\n\
            script = ExtResource( 9 )\n";
        assert_eq!(
            refs("godot-scene", src),
            vec![
                (
                    2,
                    path("inherits", "res://base.tscn", None, PathKind::Extends)
                ),
                (
                    5,
                    Spec::ParseError("ExtResource(\"9\") has no [ext_resource]".into())
                ),
            ]
        );
    }

    #[test]
    fn resources_attach_scripts() {
        let src = "[gd_resource type=\"Resource\" script_class=\"Stats\" format=3]\n\
            [ext_resource type=\"Script\" path=\"res://stats.gd\" id=\"1\"]\n\
            [resource]\nscript = ExtResource(\"1\")\nhp = 3\n";
        assert_eq!(
            refs("godot-resource", src),
            vec![(2, path("script", "res://stats.gd", None, PathKind::Direct))]
        );
    }

    #[test]
    fn project_main_scene_autoloads_and_settings() {
        let src = "config_version=5\n[application]\nrun/main_scene=\"uid://m\"\n\
            config/icon=\"res://icon.svg\"\n[autoload]\nEvents=\"*res://events.gd\"\n\
            Music=\"res://music.tscn\"\n[input]\nui_x={\n\"events\": [Object(InputEventKey,\"keycode\":0)]\n}\n";
        assert_eq!(
            refs("godot-project", src),
            vec![
                (3, path("main_scene", "uid://m", None, PathKind::Direct)),
                (
                    4,
                    path("config/icon", "res://icon.svg", None, PathKind::String)
                ),
                (
                    6,
                    Spec::Godot(Ref::Autoload {
                        name: "Events".into(),
                        path: "res://events.gd".into()
                    })
                ),
                (
                    7,
                    Spec::Godot(Ref::Autoload {
                        name: "Music".into(),
                        path: "res://music.tscn".into()
                    })
                ),
            ]
        );
    }
}
