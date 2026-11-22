; Import extraction for C++ (`.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh`, `.hxx`). Same captures
; as c.scm (see `imports/cfamily.rs`).

(preproc_include
  path: (_) @include) @import
