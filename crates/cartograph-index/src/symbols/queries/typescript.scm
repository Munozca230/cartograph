; TypeScript / TSX additions to `javascript.scm`.

(abstract_class_declaration name: (_) @name) @def.class
(interface_declaration name: (_) @name) @def.interface
(type_alias_declaration name: (_) @name) @def.type
(enum_declaration name: (_) @name) @def.enum
(public_field_definition
  name: (_) @name
  value: [(arrow_function) (function_expression)]) @def.method

; `await x.f<T>(…)`: the grammar reads `await x.f` as the function of a call with type
; arguments (`f<T>` binds tighter than `await`); the call is still `x.f()`.
(call_expression
  function: (await_expression
    (member_expression object: (_) @recv property: [(property_identifier) (private_property_identifier)] @name))
  arguments: (arguments) @args) @call.member
(call_expression function: (await_expression (identifier) @name)) @call
