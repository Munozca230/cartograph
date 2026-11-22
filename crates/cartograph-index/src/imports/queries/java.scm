; Import extraction for Java.
;
; Capture names (see `imports/mod.rs` and `imports/dotted.rs`):
;   @import   one of
;             - an `import_declaration` (`import a.b.C;`, `import a.b.*;`,
;               `import static a.b.C.m;`, `import static a.b.C.*;`);
;             - the `package_declaration` (not an import: indexed by the resolver);
;             - a top-level type declaration (not an import: its fully qualified name is
;               indexed, so a class in a file named differently is still found).
;   `dotted.rs` reads the `static` keyword, the name and the `asterisk` from the node.

(import_declaration) @import

(package_declaration) @import

(program
  [
    (class_declaration)
    (interface_declaration)
    (enum_declaration)
    (record_declaration)
    (annotation_type_declaration)
  ] @import)
