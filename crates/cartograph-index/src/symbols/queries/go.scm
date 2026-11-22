; N2 symbols for Go. Captures: see `javascript.scm`, plus
;   @recv.name / @recv.type   a method's receiver variable and type
;   @def.typespec             a named type (struct, interface or other, read from `type`)
;   @cobra                    a composite literal (`cobra.Command{Use: …}` → CLI command)

(function_declaration name: (identifier) @name) @def.function
(method_declaration
  receiver: (parameter_list
    (parameter_declaration name: (identifier)? @recv.name type: (_) @recv.type))
  name: (field_identifier) @name) @def.method
(type_spec name: (type_identifier) @name) @def.typespec
(source_file (const_declaration (const_spec name: (identifier) @name) @def.const))

(call_expression function: (identifier) @name) @call
(call_expression
  function: (selector_expression operand: (_) @recv field: (field_identifier) @name)
  arguments: (argument_list) @args) @call.member

(import_spec) @import
(composite_literal type: (qualified_type) body: (literal_value)) @cobra
