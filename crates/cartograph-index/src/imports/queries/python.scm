; Import extraction for Python. Matches at any depth: imports inside functions, classes,
; `if TYPE_CHECKING:` and `try:` blocks are found too.
;
; Capture names (see `imports/mod.rs`):
;   @import   the statement or call; its start line is recorded.
;   @module   `import a.b` / `import a.b as c`: one module per capture.
;   @from     `from <module> import ...`: a dotted name or a relative import (`..pkg`).
;   @name     an imported name of a `from` import (may be a submodule).
;   @future   `from __future__ import ...`.
;   @dynamic  first argument of __import__() / importlib.import_module(); a plain string
;             literal is a normal import, anything else is unresolved with reason `dynamic`.
; Captures starting with `_` are only used by predicates.

(import_statement
  name: (dotted_name) @module) @import

(import_statement
  name: (aliased_import
    name: (dotted_name) @module)) @import

(import_from_statement
  module_name: (_) @from) @import

(import_from_statement
  name: (dotted_name) @name) @import

(import_from_statement
  name: (aliased_import
    name: (dotted_name) @name)) @import

(future_import_statement) @future @import

(call
  function: (identifier) @_fn
  arguments: (argument_list (_) @dynamic)
  (#eq? @_fn "__import__")) @import

(call
  function: (attribute
    object: (identifier) @_obj
    attribute: (identifier) @_fn)
  arguments: (argument_list (_) @dynamic)
  (#eq? @_obj "importlib")
  (#eq? @_fn "import_module")) @import
