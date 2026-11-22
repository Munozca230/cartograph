; GDScript references (tree-sitter-gdscript). Read by `godot::gdscript`; captures:
;   @extends.path   the string of `extends "res://base.gd"` (also `extends "res://a.gd".Inner`)
;   @extends.name   the class of `extends Name` (also the head of `extends Name.Inner`)
;   @class.decl     the name of `class_name Name`
;   @local          names declared in the file (const, inner class, enum): they shadow
;                   global class names
;   @load.fn        `preload` / `load`, or `load` / `load_threaded_request` after @load.obj
;   @load.obj       the object of a method call (`ResourceLoader`)
;   @load.arg       the first argument of that call
;   @string         every string literal (only `res://` / `uid://` ones become references)
;   @ident          every identifier starting with an upper-case letter (only names of
;                   project classes and autoloads become references)

(extends_statement (string) @extends.path)
(extends_statement (type (identifier) @extends.name))
(extends_statement (type (attribute . (string) @extends.path)))
(extends_statement (type (attribute . (identifier) @extends.name)))

(class_name_statement name: (name) @class.decl)

(const_statement name: (name) @local)
(class_definition name: (name) @local)
(enum_definition name: (name) @local)

(call
  (identifier) @load.fn
  arguments: (arguments . (_) @load.arg))

(attribute
  (identifier) @load.obj
  (attribute_call
    (identifier) @load.fn
    arguments: (arguments . (_) @load.arg)))

(string) @string

((identifier) @ident
  (#match? @ident "^[A-Z]"))
