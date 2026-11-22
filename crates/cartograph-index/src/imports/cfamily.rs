//! C / C++ import extraction: `#include "x.h"` (quote form) and `#include <x>` (angle
//! form). A macro include (`#include CONFIG_H`) is [`Spec::Dynamic`].

use super::{Family, RawImport, Spec};
use crate::grammars::Grammar;
use std::collections::HashSet;
use tree_sitter::{Node, QueryCursor, StreamingIterator};

/// Extract the `#include` directives of a C or C++ file.
pub(super) fn extract(
    grammar: Grammar,
    tree: &tree_sitter::Tree,
    src: &[u8],
) -> Result<Vec<RawImport>, String> {
    let query = super::query(grammar)?;
    let names = query.capture_names();

    let mut found: Vec<(usize, RawImport)> = Vec::new();
    let mut covered = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let (mut stmt, mut path) = (None, None);
        for c in m.captures() {
            match names.get(c.index as usize).copied().unwrap_or("") {
                "import" => stmt = Some(c.node),
                "include" => path = Some(c.node),
                _ => {}
            }
        }
        let (Some(stmt), Some(path)) = (stmt, path) else {
            continue;
        };
        if !seen.insert(stmt.id()) {
            continue;
        }
        covered.push(stmt.byte_range());
        found.push((
            stmt.start_byte(),
            RawImport {
                line: super::line_of(stmt),
                spec: include_spec(path, src),
            },
        ));
    }
    found.sort_by_key(|(at, _)| *at);
    let mut out: Vec<RawImport> = found.into_iter().map(|(_, r)| r).collect();
    if tree.root_node().has_error() {
        out.extend(super::rust::error_imports(
            tree,
            src,
            Family::CFamily,
            &covered,
        ));
        out.sort_by_key(|r| r.line);
    }
    Ok(out)
}

fn include_spec(path: Node<'_>, src: &[u8]) -> Spec {
    match path.kind() {
        "string_literal" => {
            let mut s = String::new();
            let mut c = path.walk();
            for child in path.named_children(&mut c) {
                if matches!(child.kind(), "string_content" | "escape_sequence") {
                    s.push_str(&super::text(child, src));
                }
            }
            Spec::Include {
                path: s,
                system: false,
            }
        }
        "system_lib_string" => {
            let t = super::text(path, src);
            let inner = t
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .trim();
            Spec::Include {
                path: inner.to_string(),
                system: true,
            }
        }
        _ => Spec::Dynamic(super::snippet(path, src)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(lang: &str, src: &str) -> Vec<(u32, Spec)> {
        super::super::extract(lang, src.as_bytes())
            .into_iter()
            .map(|r| (r.line, r.spec))
            .collect()
    }

    fn inc(p: &str, system: bool) -> Spec {
        Spec::Include {
            path: p.into(),
            system,
        }
    }

    #[test]
    fn include_forms() {
        let src = "#include \"util.h\"\n#include <stdio.h>\n#ifdef X\n#  include <sys/types.h>\n\
            #endif\n#include CONFIG_H\n#include \"..\\win\\path.h\"\n";
        let want = vec![
            (1, inc("util.h", false)),
            (2, inc("stdio.h", true)),
            (4, inc("sys/types.h", true)),
            (6, Spec::Dynamic("CONFIG_H".into())),
            (7, inc("..\\win\\path.h", false)),
        ];
        assert_eq!(specs("c", src), want);
        assert_eq!(specs("cpp", src), want);
    }

    #[test]
    fn cpp_header_parsed_as_c_keeps_its_includes() {
        // `.h` is detected as C; C++ content must not hide includes or invent errors.
        let src = "#pragma once\n#include <vector>\n#include \"engine/core.hpp\"\n\
            namespace eng {\ntemplate <typename T> class Pool : public Base<T> {\n\
            public:\n  std::vector<T> items;\n};\n}\n";
        let got = specs("c", src);
        assert_eq!(
            got,
            vec![(2, inc("vector", true)), (3, inc("engine/core.hpp", false))]
        );
    }
}
