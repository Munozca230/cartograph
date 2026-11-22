; Import extraction for Go.
;
; Capture names (see `imports/mod.rs` and `imports/dotted.rs`):
;   @import   one `import_spec` (`"fmt"`, `m "x.org/y"`, `_ "embed"`, `. "a/b"`), single or
;             inside an `import ( ... )` block. `dotted.rs` reads its `path` field and
;             removes the quotes or backticks. The alias does not change resolution.

(import_spec) @import
