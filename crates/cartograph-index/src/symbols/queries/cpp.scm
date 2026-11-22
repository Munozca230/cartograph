; N2 symbols for C++: `c.scm` plus classes, qualified calls and `new`. An out-of-class
; definition `void Foo::bar()` is the method `Foo.bar`.

(class_specifier name: (type_identifier) @name body: (_)) @def.class
(call_expression function: (template_function name: (identifier) @name)) @call
(call_expression
  function: (qualified_identifier scope: (_) @recv name: (identifier) @name)) @call.path
(call_expression
  function: (qualified_identifier
    scope: (_) @recv name: (template_function name: (identifier) @name))) @call.path
(new_expression type: (type_identifier) @name) @call.new
