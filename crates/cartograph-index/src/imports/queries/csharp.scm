; Import extraction for C#.
;
; Capture names (see `imports/mod.rs` and `imports/dotted.rs`):
;   @import   one of
;             - a `using_directive` (`using X.Y;`, `using static X.Y.C;`,
;               `using A = X.Y;`, `global using X;`), at top level or inside a namespace;
;             - a namespace declaration, block or file-scoped (not an import: indexed by
;               the resolver; nested blocks compose, `namespace A { namespace B {} }` is
;               `A.B`);
;             - a type declared directly in a namespace or at top level (not an import:
;               its fully qualified name is indexed for `using static` and type aliases).
;   `dotted.rs` reads `global`, `static`, the alias (`name:` field) and the target name.
;   `using` statements inside methods (`using (var x = ...)`) are `using_statement` nodes
;   and never match.

(using_directive) @import

(namespace_declaration) @import

(file_scoped_namespace_declaration) @import

(compilation_unit
  [
    (class_declaration)
    (struct_declaration)
    (interface_declaration)
    (enum_declaration)
    (record_declaration)
    (delegate_declaration)
  ] @import)

(namespace_declaration
  body: (declaration_list
    [
      (class_declaration)
      (struct_declaration)
      (interface_declaration)
      (enum_declaration)
      (record_declaration)
      (delegate_declaration)
    ] @import))
