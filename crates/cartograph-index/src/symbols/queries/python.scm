; N2 symbols for Python. Captures: see `javascript.scm`, plus
;   @def.const     a module-level assignment (kept when the name is UPPER_CASE)
;   @entry.main    `if __name__ == "__main__":`
;   @decorator     a decorator (routes, CLI commands)

(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.class
(module (expression_statement (assignment left: (identifier) @name)) @def.const)

(call function: (identifier) @name) @call
(call
  function: (attribute object: (_) @recv attribute: (identifier) @name)
  arguments: (argument_list) @args) @call.member

(import_statement) @import
(import_from_statement) @import
(module (if_statement condition: (comparison_operator) @cond) @entry.main)
(decorator) @decorator
