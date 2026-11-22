//! `findings(filter?)`: the findings of the repo, filtered, within [`limits::FINDINGS`].

use crate::budget::{Answer, clip, fit, limits};
use crate::findings::{Finding, FindingKind, Severity, Summary, summarize};
use crate::graph::{NodeRef, RepoGraph};
use crate::{QueryError, Repo};
use serde::Serialize;

/// Which findings to list.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Only these kinds (all when empty).
    pub kinds: Vec<FindingKind>,
    /// At least this severity.
    pub severity: Option<Severity>,
    /// Only findings that touch this node (a file, or anything under a dir).
    pub within: Option<String>,
    /// Only rule violations.
    pub violations: bool,
}

/// A finding cut down for a listing.
#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub id: String,
    pub kind: FindingKind,
    pub severity: Severity,
    pub violation: bool,
    pub title: String,
    pub nodes: Vec<String>,
    pub evidence: Vec<String>,
    /// Evidence lines left out.
    pub evidence_more: usize,
}

impl Brief {
    pub fn of(f: &Finding, max_evidence: usize) -> Brief {
        Brief {
            id: f.id.clone(),
            kind: f.kind,
            severity: f.severity,
            violation: f.violation,
            title: crate::budget::clip(&f.title, 200),
            nodes: f.nodes.iter().take(3).cloned().collect(),
            evidence: f
                .evidence
                .iter()
                .take(max_evidence)
                .map(|e| crate::budget::clip(e, 200))
                .collect(),
            evidence_more: f.evidence.len().saturating_sub(max_evidence) + f.evidence_more,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingsResult {
    /// Counts over every finding that matches the filter.
    pub summary: Summary,
    pub items: Vec<Brief>,
    /// Matching findings left out.
    pub more: usize,
}

/// Whether a finding touches node `n` (the file itself, or anything under the dir).
pub fn touches(g: &RepoGraph, f: &Finding, n: NodeRef) -> bool {
    let within = |id: &str| -> bool {
        let Some(p) = id.strip_prefix("file:").or_else(|| id.strip_prefix("dir:")) else {
            return false;
        };
        match n {
            NodeRef::File(x) => g.path(x) == p,
            NodeRef::Dir(d) => {
                let dp = &g.dirs[d as usize].path;
                dp.is_empty()
                    || p == dp
                    || (p.starts_with(dp.as_str()) && p.as_bytes().get(dp.len()) == Some(&b'/'))
            }
            NodeRef::Ext(_) => false,
        }
    };
    f.nodes.iter().any(|x| within(x)) || f.edges.iter().any(|e| within(&e.src) || within(&e.dst))
}

/// One finding as text: `[severity] id` and its title, then evidence lines indented.
pub fn render_brief(b: &Brief, out: &mut String) {
    let tag = if b.violation {
        "VIOLATION"
    } else {
        b.severity.as_str()
    };
    out.push_str(&format!(
        "[{tag}] {}\n  {}\n",
        clip(&b.id, 160),
        clip(&b.title, 200)
    ));
    for e in &b.evidence {
        out.push_str(&format!("    {}\n", clip(e, 200)));
    }
    if b.evidence_more > 0 {
        out.push_str(&format!("    +{} more\n", b.evidence_more));
    }
}

pub fn summary_line(s: &Summary) -> String {
    if s.total == 0 {
        return "no findings".to_string();
    }
    let kinds: Vec<String> = s.by_kind.iter().map(|(k, n)| format!("{k} {n}")).collect();
    let sev: Vec<String> = s
        .by_severity
        .iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    format!(
        "{} findings ({}{}): {}",
        s.total,
        if s.violations > 0 {
            format!("{} violations, ", s.violations)
        } else {
            String::new()
        },
        sev.join(", "),
        kinds.join(", ")
    )
}

pub fn findings(repo: &Repo, filter: &Filter) -> Result<Answer<FindingsResult>, QueryError> {
    let g = &repo.graph;
    let within = match &filter.within {
        Some(w) => Some(repo.resolve(w)?),
        None => None,
    };
    let all: Vec<&Finding> = repo
        .findings()
        .iter()
        .filter(|f| filter.kinds.is_empty() || filter.kinds.contains(&f.kind))
        .filter(|f| filter.severity.is_none_or(|s| f.severity <= s))
        .filter(|f| !filter.violations || f.violation)
        .filter(|f| within.is_none_or(|n| touches(g, f, n)))
        .collect();
    let summary = summarize(all.iter().copied());
    Ok(fit(
        limits::FINDINGS,
        &[30, 24, 18, 14, 10, 8, 6, 4, 3, 2, 1],
        |limit| {
            let ev = (40 / limit.max(1)).clamp(2, 8);
            FindingsResult {
                summary: summary.clone(),
                items: all.iter().take(limit).map(|f| Brief::of(f, ev)).collect(),
                more: all.len().saturating_sub(limit),
            }
        },
        |r| {
            let mut s = format!("{}\n", summary_line(&r.summary));
            for b in &r.items {
                render_brief(b, &mut s);
            }
            if r.more > 0 {
                s.push_str(&format!(
                    "+{} more (narrow with --kind, --severity or --within)\n",
                    r.more
                ));
            }
            s
        },
    ))
}
