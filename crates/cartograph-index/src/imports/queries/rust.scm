; Import extraction for Rust. Matches at any depth: `use` inside functions and inline
; modules is found too. The enclosing inline modules, `#[path]` attributes and the
; flattening of `use a::{b, c::d}` trees are handled in `imports/rust.rs`.
;
; Capture names (see `imports/rust.rs`):
;   @import   the item; its start line is recorded.
;   @use      the argument of a `use` declaration (a path, a `{…}` group, `x as y`, `x::*`).
;   @mod      the name of a `mod` item. With a body (`mod x { … }`) it is a declaration
;             of an inline module, not an import.
;   @crate    the name of an `extern crate` item; @alias its `as` name.

(use_declaration
  argument: (_) @use) @import

(mod_item
  name: (identifier) @mod) @import

(extern_crate_declaration
  name: (identifier) @crate
  alias: (identifier)? @alias) @import
