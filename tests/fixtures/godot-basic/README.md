# godot-basic fixture

Two Godot projects in one repo (`game/` and `editor/`) and a loose script outside both.
`game/` has autoloads (a script and a scene), a main scene given by uid that instances a
sub-scene and an inherited scene, attached scripts, `class_name` inheritance, `uid://`
references (through `.uid`, `.import` and scene headers), dynamic loads, broken paths, a
binary texture, a duplicated class name and a wrong-case path. `game/crlf/` keeps CRLF.
`game/.godot/` stands for the editor cache, which is never walked.
