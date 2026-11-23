//! Paths typed by a user or an agent in a workspace (several repositories indexed as one,
//! each a top-level folder named after its member).
//!
//! Accepted besides the canonical path (`api/src/main.ts`): the alias `api:src/main.ts`
//! (with or without an id kind: `file:api:src/main.ts`, `sym:api:src/a.ts#f`), and a
//! plain path relative to a member (`src/main.ts`). A plain path that is not a path of the
//! graph is looked for in the default member (`--member`), then in every member: in one it
//! resolves, in several it is [`QueryError::AmbiguousPath`] listing them as aliases.
//! Outputs always print canonical paths.

use crate::QueryError;
use cartograph_core::expand_alias;

/// The members of a workspace index, and the one plain paths try first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Members {
    /// Member names, sorted. Empty for a single repo (everything below is then a no-op).
    pub names: Vec<String>,
    /// The member a plain path is looked for in first (`--member`).
    pub default: Option<String>,
}

const KINDS: [&str; 4] = ["file:", "dir:", "sym:", ""];

impl Members {
    pub fn is_workspace(&self) -> bool {
        !self.names.is_empty()
    }

    /// The member a path of the graph lies in.
    pub fn member_of<'p>(&self, path: &'p str) -> Option<&'p str> {
        let first = path.split('/').next()?;
        self.names.iter().any(|n| n == first).then_some(first)
    }

    /// `input` with any alias or member-relative path spelled canonically (see the module
    /// docs); `exists` says whether a path of the graph is a file or a folder. Anything else
    /// (a canonical path, a name, an external id, a path found nowhere) comes back as it
    /// was, for the caller to resolve or report.
    pub fn canonical(
        &self,
        input: &str,
        exists: impl Fn(&str) -> bool,
    ) -> Result<String, QueryError> {
        let t = input.trim();
        if !self.is_workspace() {
            return Ok(t.to_string());
        }
        if let Some(x) = expand_alias(t, &self.names) {
            return Ok(x);
        }
        let Some((kind, rest)) = KINDS
            .iter()
            .find_map(|k| t.strip_prefix(k).map(|r| (*k, r)))
        else {
            return Ok(t.to_string());
        };
        let rest = rest.replace('\\', "/");
        let rest = rest.strip_prefix("./").unwrap_or(&rest);
        // An external id (`npm:react`) or a drive letter is no member path.
        if rest.contains(':') {
            return Ok(t.to_string());
        }
        let (path, frag) = match (kind, rest.find('#')) {
            ("sym:" | "", Some(i)) => rest.split_at(i),
            _ => (rest, ""),
        };
        let p = path.trim_end_matches('/');
        if p.is_empty() || p == "." || exists(p) || self.member_of(p).is_some() {
            return Ok(t.to_string());
        }
        let moved = |m: &str| format!("{kind}{m}/{path}{frag}");
        if let Some(d) = &self.default
            && exists(&format!("{d}/{p}"))
        {
            return Ok(moved(d));
        }
        let hits: Vec<&String> = self
            .names
            .iter()
            .filter(|m| exists(&format!("{m}/{p}")))
            .collect();
        match hits.as_slice() {
            [] => Ok(t.to_string()),
            [m] => Ok(moved(m)),
            many => Err(QueryError::AmbiguousPath {
                input: t.to_string(),
                candidates: many.iter().map(|m| format!("{m}:{p}{frag}")).collect(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Members {
        Members {
            names: vec!["api".into(), "desktop".into(), "mcp".into()],
            default: None,
        }
    }

    const PATHS: [&str; 6] = [
        "api/src/main.ts",
        "desktop/src/main.ts",
        "desktop/src/ui",
        "mcp/src/tools.ts",
        "api/src",
        "desktop/src",
    ];

    fn exists(p: &str) -> bool {
        PATHS.contains(&p)
    }

    #[test]
    fn aliases_plain_paths_and_ambiguity() {
        let m = ws();
        let c = |s: &str| m.canonical(s, exists);
        assert_eq!(c("api:src/main.ts").unwrap(), "api/src/main.ts");
        assert_eq!(
            c("sym:mcp:src/tools.ts#f").unwrap(),
            "sym:mcp/src/tools.ts#f"
        );
        assert_eq!(c("src/tools.ts").unwrap(), "mcp/src/tools.ts");
        assert_eq!(c("file:src/tools.ts").unwrap(), "file:mcp/src/tools.ts");
        assert_eq!(c("src/tools.ts#lookup").unwrap(), "mcp/src/tools.ts#lookup");
        assert_eq!(c("dir:src/ui/").unwrap(), "dir:desktop/src/ui/");
        assert_eq!(c("api/src/main.ts").unwrap(), "api/src/main.ts");
        assert_eq!(c("npm:react").unwrap(), "npm:react");
        assert_eq!(c("UserController.get").unwrap(), "UserController.get");
        assert_eq!(c("nowhere.ts").unwrap(), "nowhere.ts");
        match c("src/main.ts") {
            Err(QueryError::AmbiguousPath { candidates, .. }) => {
                assert_eq!(candidates, vec!["api:src/main.ts", "desktop:src/main.ts"]);
            }
            other => panic!("{other:?}"),
        }
        let d = Members {
            default: Some("desktop".into()),
            ..ws()
        };
        assert_eq!(
            d.canonical("src/main.ts", exists).unwrap(),
            "desktop/src/main.ts"
        );
        let single = Members::default();
        assert_eq!(single.canonical(" src/x ", exists).unwrap(), "src/x");
    }
}
