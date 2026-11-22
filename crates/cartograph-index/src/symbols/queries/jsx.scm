; JSX (JavaScript and TSX): rendering `<Comp/>` counts as a call of `Comp`.

(jsx_opening_element name: (identifier) @name) @call
(jsx_self_closing_element name: (identifier) @name) @call
(jsx_opening_element
  name: (member_expression object: (identifier) @recv property: (property_identifier) @name)) @call.member
(jsx_self_closing_element
  name: (member_expression object: (identifier) @recv property: (property_identifier) @name)) @call.member
