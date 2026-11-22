//! Repo-relative path helpers that import resolution ([`crate::resolve`]) and call
//! resolution ([`crate::symbols`]) share. A leaf: it depends on neither.

/// Whether `dir` is `path` itself or one of its ancestors (`""` is everyone's ancestor).
pub fn is_ancestor(dir: &str, path: &str) -> bool {
    dir.is_empty()
        || path == dir
        || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/')
}
