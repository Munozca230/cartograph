; N2 symbols for C. Captures: see `javascript.scm`, plus
;   @def.function + @decl   a function definition; the name is read from the declarator
; Declarations without a body (prototypes) are not symbols: a call resolves to the
; definition, found through the included header's sibling source file.

(function_definition declarator: (_) @decl) @def.function
(struct_specifier name: (type_identifier) @name body: (_)) @def.struct
(union_specifier name: (type_identifier) @name body: (_)) @def.struct
(enum_specifier name: (type_identifier) @name body: (_)) @def.enum
(type_definition declarator: (type_identifier) @name) @def.type

(call_expression function: (identifier) @name) @call
(call_expression
  function: (field_expression argument: (_) @recv field: (field_identifier) @name)
  arguments: (argument_list) @args) @call.member
