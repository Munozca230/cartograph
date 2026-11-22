; Import extraction for JavaScript / JSX / MJS / CJS (and the shared part of TypeScript).
;
; Capture names (see `imports/mod.rs`):
;   @import   the statement or call; its start line is recorded.
;   @source   a string literal holding the specifier.
;   @dynamic  an argument of require()/import(); a plain string literal is a normal import,
;             anything else is recorded as unresolved with reason `dynamic`.
; Captures starting with `_` are only used by predicates.

; import x from 'a';  import 'a';  import type { T } from 'a';  import * as ns from 'a';
(import_statement
  source: (string) @source) @import

; export { a } from 'a';  export * from 'a';  export * as ns from 'a';  export type { T } from 'a';
(export_statement
  source: (string) @source) @import

; require('a'), require(`a`), require(name)
(call_expression
  function: (identifier) @_fn
  arguments: (arguments (_) @dynamic)
  (#eq? @_fn "require")) @import

; import('a'), import(name)
(call_expression
  function: (import)
  arguments: (arguments (_) @dynamic)) @import
