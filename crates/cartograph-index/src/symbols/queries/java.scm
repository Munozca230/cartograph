; N2 symbols for Java. Captures: see `javascript.scm`. Constructors are not symbols:
; `new T()` is a call of the class `T`.

(class_declaration name: (identifier) @name) @def.class
(record_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(annotation_type_declaration name: (identifier) @name) @def.interface
(enum_declaration name: (identifier) @name) @def.enum
(method_declaration name: (identifier) @name) @def.method

(method_invocation !object name: (identifier) @name) @call
(method_invocation object: (_) @recv name: (identifier) @name) @call.member
(object_creation_expression type: (type_identifier) @name) @call.new
(object_creation_expression type: (generic_type (type_identifier) @name)) @call.new

(import_declaration) @import
