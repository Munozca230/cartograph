; N2 symbols for JavaScript / JSX (TypeScript and TSX add `typescript.scm`).
; Captures read by `symbols/extract.rs`:
;   @def.<kind> + @name   a definition (kind: function, method, class, interface, enum, type, var)
;   @call + @name         `f()`, `<Comp/>`
;   @call.member + @name (+ @recv, @args)   `x.f()`, `this.f()`, `<ui.Panel/>`
;   @call.new + @name (+ @recv)              `new F()`, `new ns.F()`
;   @import               an import, a re-export or a `require` binding
;   @export               an export statement (default export, `export { a }`)
;   @decorator            a decorator (route detection)

(function_declaration name: (_) @name) @def.function
(generator_function_declaration name: (_) @name) @def.function
(class_declaration name: (_) @name) @def.class
(method_definition name: (_) @name) @def.method
(variable_declarator name: (identifier) @name value: (_)) @def.var

(call_expression function: (identifier) @name) @call
(call_expression
  function: (member_expression object: (_) @recv property: [(property_identifier) (private_property_identifier)] @name)
  arguments: (arguments) @args) @call.member
(new_expression constructor: (identifier) @name) @call.new
(new_expression
  constructor: (member_expression object: (identifier) @recv property: (property_identifier) @name)) @call.new

(import_statement) @import
(export_statement source: (_)) @import
(variable_declarator
  value: (call_expression function: (identifier) @_req (#eq? @_req "require"))) @import
(export_statement) @export
(decorator) @decorator
