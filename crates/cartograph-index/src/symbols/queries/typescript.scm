; TypeScript / TSX additions to `javascript.scm`.

(abstract_class_declaration name: (_) @name) @def.class
(interface_declaration name: (_) @name) @def.interface
(type_alias_declaration name: (_) @name) @def.type
(enum_declaration name: (_) @name) @def.enum
(public_field_definition
  name: (_) @name
  value: [(arrow_function) (function_expression)]) @def.method
