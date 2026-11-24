//! `check`: the findings that break a rule of `cartograph.toml` (layer violations, cycles in
//! `no_cycles` scopes, `max_fan_out`). Those fail the check; everything else is
//! informational.
//!
//! With a baseline: [`check_with`] takes the findings of a baseline and fails only on
//! violations whose id it lacks; [`check_base`] (`check --base`) compares with the same repo
//! at a git base and fails only on the violations the change introduces.

use crate::Repo;
use crate::budget::{Answer, clip, fit, limits};
use crate::diff::{self, Gone};
use crate::findings::{Finding, Summary, summarize};
use crate::list::{Brief, render_brief, summary_line};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// No violation (new violation, with a baseline).
    pub ok: bool,
    /// Where the rules came from; `None` = no `cartograph.toml`.
    pub config: Option<String>,
    /// Whether the config declares any rule.
    pub rules: bool,
    /// The git base compared with (`main (1a2b3c4d)`), for `check --base`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Violations that fail the check. With a base: the ones the change introduces, an
    /// existing one that got worse showing only its new evidence.
    pub violations: Vec<Brief>,
    pub violations_more: usize,
    /// Violations already in the baseline (they do not fail the check).
    pub baseline_violations: usize,
    /// Some of them (with a base).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub existing: Vec<Gone>,
    /// Violations of the base that the change removed.
    #[serde(skip_serializing_if = "is_zero")]
    pub fixed: usize,
    /// Counts of the other findings (informational).
    pub informational: Summary,
    /// What the change breaks in the contracts (with a base; never fails the check).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contract_breaks: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub contract_breaks_more: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// `check` against the current findings, without a baseline.
pub fn check(repo: &Repo) -> Answer<CheckResult> {
    check_with(repo, repo.findings(), None)
}

/// `check` over `findings`; with `baseline`, violations whose id the baseline has too are
/// counted apart and do not fail.
pub fn check_with(
    repo: &Repo,
    findings: &[Finding],
    baseline: Option<&[Finding]>,
) -> Answer<CheckResult> {
    let old: BTreeSet<&str> = baseline
        .unwrap_or(&[])
        .iter()
        .filter(|f| f.violation)
        .map(|f| f.id.as_str())
        .collect();
    let (violations, rest): (Vec<&Finding>, Vec<&Finding>) =
        findings.iter().partition(|f| f.violation);
    let (old_v, new_v): (Vec<&Finding>, Vec<&Finding>) = violations
        .into_iter()
        .partition(|f| old.contains(f.id.as_str()));
    let new_v: Vec<Brief> = new_v.iter().map(|f| Brief::of(f, 4)).collect();
    let informational = summarize(rest.iter().copied());
    build(repo, None, &new_v, &old_v, 0, informational, &[])
}

/// `check --base`: only the violations that `repo` (the working tree) introduces relative
/// to `base` (the same repo at a git base, built with the same config) fail: new ones, and
/// existing ones that now rest on a dependency the base lacks (see [`diff::compare`]). The
/// others are reported as existing.
pub fn check_base(repo: &Repo, base: &Repo, base_label: &str) -> Answer<CheckResult> {
    let compared = diff::compare(base, repo);
    let introduced: Vec<Brief> = compared
        .introduced()
        .map(|(f, c)| diff::brief(repo, f, c, 4))
        .collect();
    let existing: Vec<&Finding> = compared
        .head
        .iter()
        .filter(|(f, c)| f.violation && matches!(c, diff::Change::Same))
        .map(|(f, _)| *f)
        .collect();
    let fixed = compared.resolved.iter().filter(|f| f.violation).count();
    let informational = summarize(repo.findings().iter().filter(|f| !f.violation));
    let breaks = diff::contract_breaks(base, repo, &compared);
    build(
        repo,
        Some(base_label.to_string()),
        &introduced,
        &existing,
        fixed,
        informational,
        &breaks,
    )
}

fn build(
    repo: &Repo,
    base: Option<String>,
    new_v: &[Brief],
    old_v: &[&Finding],
    fixed: usize,
    informational: Summary,
    breaks: &[String],
) -> Answer<CheckResult> {
    let config = repo.config.origin.clone();
    let rules = repo.config.has_rules();
    let with_base = base.is_some();
    fit(
        limits::CHECK,
        &[20, 14, 10, 8, 6, 4, 3, 2, 1],
        |limit| CheckResult {
            ok: new_v.is_empty(),
            config: config.clone(),
            rules,
            base: base.clone(),
            violations: new_v.iter().take(limit).cloned().collect(),
            violations_more: new_v.len().saturating_sub(limit),
            baseline_violations: old_v.len(),
            existing: if with_base {
                old_v
                    .iter()
                    .take(limit.min(6))
                    .map(|f| Gone {
                        id: f.id.clone(),
                        violation: true,
                        title: clip(&f.title, 160),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            fixed,
            informational: informational.clone(),
            contract_breaks: breaks
                .iter()
                .take(limit.min(6))
                .map(|b| clip(b, 300))
                .collect(),
            contract_breaks_more: breaks.len().saturating_sub(limit.min(6)),
        },
        render,
    )
}

fn render(r: &CheckResult) -> String {
    let mut s = String::new();
    match (&r.config, r.rules) {
        (None, _) => s.push_str("no cartograph.toml: no rules to check (see `cartograph infer`)\n"),
        (Some(c), false) => s.push_str(&format!("{c}: no rules declared\n")),
        (Some(c), true) => s.push_str(&format!("rules from {c}\n")),
    }
    if let Some(b) = &r.base {
        s.push_str(&format!(
            "base: {b}; only violations the change introduces fail\n"
        ));
    }
    let what = if r.base.is_some() {
        "new rule violations"
    } else {
        "rule violations"
    };
    if r.ok {
        s.push_str(&format!("PASS: no {what}\n"));
    } else {
        s.push_str(&format!(
            "FAIL: {} {what}\n",
            r.violations.len() + r.violations_more
        ));
    }
    for b in &r.violations {
        render_brief(b, &mut s);
    }
    if r.violations_more > 0 {
        s.push_str(&format!(
            "+{} more violations (`cartograph query findings --violations`)\n",
            r.violations_more
        ));
    }
    if r.baseline_violations > 0 {
        if r.base.is_some() {
            s.push_str(&format!(
                "existing (in the base too, not failing): {}\n",
                r.baseline_violations
            ));
            for g in &r.existing {
                s.push_str(&format!("  {}: {}\n", clip(&g.id, 160), g.title));
            }
            if r.baseline_violations > r.existing.len() {
                s.push_str(&format!(
                    "  +{} more\n",
                    r.baseline_violations - r.existing.len()
                ));
            }
        } else {
            s.push_str(&format!(
                "{} violations already in the baseline\n",
                r.baseline_violations
            ));
        }
    }
    if r.fixed > 0 {
        s.push_str(&format!("fixed since the base: {} violations\n", r.fixed));
    }
    s.push_str(&format!(
        "informational: {}\n",
        summary_line(&r.informational)
    ));
    if !r.contract_breaks.is_empty() {
        s.push_str("not failing: ");
        diff::render_breaks(&mut s, &r.contract_breaks, r.contract_breaks_more);
    }
    s
}
