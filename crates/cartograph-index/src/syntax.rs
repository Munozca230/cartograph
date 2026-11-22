//! Syntax-tree helpers that import extraction ([`crate::imports`]) and symbol extraction
//! ([`crate::symbols`]) share. A leaf: it depends on neither.

use tree_sitter::Node;

/// Rust: names of the inline modules enclosing `node` (`mod tests { … }`), outermost first.
pub(crate) fn inline_scope(node: Node<'_>, src: &[u8]) -> Vec<String> {
    let mut scope = Vec::new();
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "mod_item"
            && let Some(name) = n.child_by_field_name("name")
        {
            scope.push(
                String::from_utf8_lossy(src.get(name.byte_range()).unwrap_or(&[])).into_owned(),
            );
        }
        cur = n.parent();
    }
    scope.reverse();
    scope
}
