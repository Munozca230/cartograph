//! Repo-relative path helpers that import resolution ([`crate::resolve`]) and call
//! resolution ([`crate::symbols`]) share. A leaf: it depends on neither.

/// Whether `dir` is `path` itself or one of its ancestors (`""` is everyone's ancestor).
pub fn is_ancestor(dir: &str, path: &str) -> bool {
    dir.is_empty()
        || path == dir
        || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/')
}

/// `rel` (relative to a mount root) as a path of the graph: below `prefix`, the mount's
/// folder in the graph (`""` = the repo root, where `rel` is returned unchanged).
pub fn under(prefix: &str, rel: &str) -> String {
    match (prefix.is_empty(), rel.is_empty()) {
        (true, _) => rel.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{rel}"),
    }
}

/// `path` of the graph relative to the mount folder `prefix`, when it lies below it (or is
/// it: `""`).
pub fn strip_mount<'p>(prefix: &str, path: &'p str) -> Option<&'p str> {
    if prefix.is_empty() {
        return Some(path);
    }
    match path.strip_prefix(prefix)? {
        "" => Some(""),
        rest => rest.strip_prefix('/'),
    }
}
