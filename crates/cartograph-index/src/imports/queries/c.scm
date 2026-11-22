; Import extraction for C (`.c`, `.h`). Matches at any depth: includes inside `#if` /
; `#ifdef` blocks are found too.
;
; Capture names (see `imports/cfamily.rs`):
;   @import   the `#include` directive; its start line is recorded.
;   @include  its path: a `"…"` string_literal (quote form), a `<…>` system_lib_string
;             (angle form), or a macro (`#include CONFIG_H`), recorded as unresolved
;             `dynamic`.

(preproc_include
  path: (_) @include) @import
