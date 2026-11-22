; N2 symbols for C#. Captures: see `javascript.scm`. Constructors are not symbols:
; `new T()` is a call of the type `T`.

(class_declaration name: (identifier) @name) @def.class
(record_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(struct_declaration name: (identifier) @name) @def.struct
(enum_declaration name: (identifier) @name) @def.enum
(method_declaration name: (identifier) @name) @def.method

(invocation_expression function: (identifier) @name) @call
(invocation_expression function: (generic_name (identifier) @name)) @call
(invocation_expression
  function: (member_access_expression expression: _ @recv name: (identifier) @name)
  arguments: (argument_list) @args) @call.member
(invocation_expression
  function: (member_access_expression expression: _ @recv name: (generic_name (identifier) @name))
  arguments: (argument_list) @args) @call.member
(object_creation_expression type: (identifier) @name) @call.new
(object_creation_expression type: (generic_name (identifier) @name)) @call.new
(object_creation_expression type: (qualified_name name: (identifier) @name)) @call.new

(using_directive) @import
