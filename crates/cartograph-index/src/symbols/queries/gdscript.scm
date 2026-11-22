; N2 symbols for GDScript. Captures: see `javascript.scm`. Top-level functions belong to the
; file's class (`class_name`, or the script itself); inner classes hold their own methods.
;   @def.class_name   `class_name Name` (the file is that class)
;   @import           `const X = preload("…")` binds X to the loaded script

(function_definition name: (name) @name) @def.function
(class_definition name: (name) @name) @def.class
(class_name_statement name: (name) @name) @def.class_name
(source (const_statement name: (name) @name) @def.const)
(source (enum_definition name: (name) @name) @def.enum)

(call . (identifier) @name) @call
(attribute . (identifier) @recv . (attribute_call (identifier) @name)) @call.member
(attribute (attribute_call (identifier) @name)) @call.member

(const_statement
  name: (name)
  value: (call . (identifier) @_fn (#any-of? @_fn "preload" "load"))) @import
