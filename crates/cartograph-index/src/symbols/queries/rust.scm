; N2 symbols for Rust. Captures: see `javascript.scm`, plus
;   @call.path + @recv + @name   `Type::f()`, `module::f()`, `Self::f()`
;   @attribute                   an attribute (`#[derive(Subcommand)]` → CLI commands)

(function_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(union_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(trait_item name: (type_identifier) @name) @def.trait
(type_item name: (type_identifier) @name) @def.type
(const_item name: (identifier) @name) @def.const
(static_item name: (identifier) @name) @def.const

(call_expression function: (identifier) @name) @call
(call_expression function: (generic_function function: (identifier) @name)) @call
(call_expression
  function: (scoped_identifier path: (_) @recv name: (identifier) @name)) @call.path
(call_expression
  function: (generic_function
    function: (scoped_identifier path: (_) @recv name: (identifier) @name))) @call.path
(call_expression
  function: (field_expression value: (_) @recv field: (field_identifier) @name)
  arguments: (arguments) @args) @call.member
(call_expression
  function: (generic_function
    function: (field_expression value: (_) @recv field: (field_identifier) @name))) @call.member

; Inside a macro's arguments (`assert_eq!(f(x), 1)`, `format!("{}", a.b())`) the grammar
; keeps tokens: `name` followed by a `(…)` token tree is read as a call.
(token_tree (identifier) @name . (token_tree) @args) @call.macro

(use_declaration) @import
(mod_item name: (identifier) !body) @import
(attribute_item) @attribute
