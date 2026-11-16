//! Opt-in, versioned advisory interpretation of crawl observations.
use crate::{
    audit,
    model::{Finding, Page, Report, Severity, digest},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdvisoryPolicy {
    pub title_min: usize,
    pub title_max: usize,
    pub description_min: usize,
    pub description_max: usize,
    pub words_min: usize,
    pub links_max: usize,
    /// Only explicit expectations turn observed directives into policy violations.
    pub require_indexable: bool,
    pub forbid_redirects: bool,
}
impl Default for AdvisoryPolicy {
    fn default() -> Self {
        Self {
            title_min: 15,
            title_max: 60,
            description_min: 50,
            description_max: 160,
            words_min: 100,
            links_max: 200,
            require_indexable: false,
            forbid_redirects: false,
        }
    }
}

impl AdvisoryPolicy {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.title_min <= self.title_max && self.description_min <= self.description_max,
            "minimum must not exceed maximum"
        );
        anyhow::ensure!(
            [
                self.title_max,
                self.description_max,
                self.words_min,
                self.links_max
            ]
            .iter()
            .all(|v| *v <= 1_000_000),
            "threshold exceeds limit"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Guidance {
    pub rule: String,
    pub version: String,
    pub kind: String,
    pub applicability: String,
    pub explanation: String,
    pub evidence_schema: String,
    pub source: String,
    pub reviewed_at: String,
    pub confidence: String,
    pub fix_guidance: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyMetadata {
    pub version: String,
    pub config: AdvisoryPolicy,
    pub catalog: Vec<Guidance>,
}

pub fn catalog() -> Vec<Guidance> {
    audit::rules().into_iter().filter_map(|r| {
        let (kind, applicability, explanation, evidence, source, fix) = match r.id {
            "short_title" | "long_title" => ("advisory", "Successfully extracted HTML with a nonempty title", "Character count crosses a configured review threshold; display depends on context and there is no universal ideal length.", "page.title; Unicode scalar count", "https://developers.google.com/search/docs/appearance/title-link", "Review whether the title describes the page clearly; edit only where useful."),
            "short_description" | "long_description" => ("advisory", "Successfully extracted HTML with a nonempty description", "Character count crosses a configured review threshold; search snippets vary by query and display.", "page.description; Unicode scalar count", "https://developers.google.com/search/docs/appearance/snippet", "Review whether the description accurately summarizes the page."),
            "little_content" => ("advisory", "Successfully extracted HTML page", "Extracted word count falls below a configured review threshold; word count alone does not determine content quality.", "page.words; extracted word count", "https://developers.google.com/search/docs/fundamentals/creating-helpful-content", "Assess whether the page answers its users' needs; do not add words solely to meet a count."),
            "too_many_links" => ("advisory", "Successfully extracted HTML page", "Outgoing link count exceeds a configured review threshold; this is not a universal link limit.", "page.links; extracted link count", "https://developers.google.com/search/docs/crawling-indexing/links-crawlable", "Review whether links are useful, contextual and crawlable."),
            "noindex" => ("observation_or_policy_violation", "HTML page with an observed noindex directive; violation requires explicit indexability intent", "A noindex directive is an observation and may be intentional.", "page.robots; observed directive", "https://developers.google.com/search/docs/crawling-indexing/block-indexing", "If this page should be indexed, review its robots directives."),
            "http_redirect" => ("observation_or_policy_violation", "Observed HTTP 3xx response; violation requires explicit redirect prohibition", "A redirect is an observation and may be intentional.", "page.status; observed HTTP status", "https://developers.google.com/search/docs/crawling-indexing/301-redirects", "Review the redirect only when it conflicts with the intended URL policy."),
            "missing_hreflang_xdefault" => ("observation", "Page contains alternate language links", "An x-default alternate is optional; its absence alone is not a defect.", "page.alternates; observed hreflang values", "https://developers.google.com/search/docs/specialty/international/localized-versions", "Add x-default only if a fallback page serves the site's audience."),
            _ => return None,
        };
        Some(Guidance { rule: r.id.into(), version: "s14-1".into(), kind: kind.into(), applicability: applicability.into(), explanation: explanation.into(), evidence_schema: evidence.into(), source: source.into(), reviewed_at: "2026-11-16".into(), confidence: "observed_data_only".into(), fix_guidance: fix.into() })
    }).collect()
}

fn add(findings: &mut Vec<Finding>, page: &Page, rule: &str, evidence: String) {
    findings.push(Finding {
        key: digest(format!("{}\n{}", rule, page.url).as_bytes()),
        rule: rule.into(),
        severity: Severity::Warning,
        url: page.url.clone(),
        evidence_items: vec![evidence.clone()],
        evidence,
        details: serde_json::json!({"classification":"advisory"}),
    });
}
pub fn apply(report: &mut Report, policy: &AdvisoryPolicy) {
    let affected = [
        "short_title",
        "long_title",
        "short_description",
        "long_description",
        "little_content",
        "too_many_links",
        "noindex",
        "http_redirect",
        "missing_hreflang_xdefault",
    ];
    report
        .findings
        .retain(|f| !affected.contains(&f.rule.as_str()));
    for p in &report.pages {
        if p.html()
            && !p.is_resource
            && !p.extraction_truncated
            && p.error.is_none()
            && !p.blocked_by_robots
        {
            for (value, min, max, short, long) in [
                (
                    &p.title,
                    policy.title_min,
                    policy.title_max,
                    "short_title",
                    "long_title",
                ),
                (
                    &p.description,
                    policy.description_min,
                    policy.description_max,
                    "short_description",
                    "long_description",
                ),
            ] {
                let n = value.chars().count();
                if n > 0 && n < min {
                    add(&mut report.findings, p, short, format!("{n} characters"));
                } else if n > max {
                    add(&mut report.findings, p, long, format!("{n} characters"));
                }
            }
            if p.words < policy.words_min {
                add(
                    &mut report.findings,
                    p,
                    "little_content",
                    format!("{} words", p.words),
                );
            }
            if p.links.len() > policy.links_max {
                add(
                    &mut report.findings,
                    p,
                    "too_many_links",
                    format!("{}", p.links.len()),
                );
            }
            if policy.require_indexable && p.noindex() {
                add(&mut report.findings, p, "noindex", p.robots.clone());
                if let Some(f) = report.findings.last_mut() {
                    f.details = serde_json::json!({"classification":"policy_violation"});
                }
            }
        }
        if policy.forbid_redirects
            && let Some(status) = p.status.filter(|s| (300..400).contains(s))
        {
            add(&mut report.findings, p, "http_redirect", status.to_string());
            if let Some(f) = report.findings.last_mut() {
                f.details = serde_json::json!({"classification":"policy_violation"});
            }
        }
    }
    report
        .findings
        .sort_by(|a, b| (&a.url, &a.rule).cmp(&(&b.url, &b.rule)));
    // Experimental schema must not be interpreted as a legacy report by HTP.
    report.schema_version = 3;
    report.policy_guidance = Some(PolicyMetadata {
        version: "s14-1".into(),
        config: policy.clone(),
        catalog: catalog(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report() -> Report {
        serde_json::from_str(include_str!("../fixtures/contracts/legacy.json")).unwrap()
    }
    #[test]
    fn thresholds_and_explicit_directive_policy_are_distinct() {
        let mut r = report();
        let p = &mut r.pages[0];
        p.status = Some(200);
        p.content_type = "text/html".into();
        p.robots = "noindex".into();
        p.title = "short".into();
        p.words = 5;
        p.extraction_truncated = false;
        p.error = None;
        p.blocked_by_robots = false;
        let mut config = AdvisoryPolicy::default();
        apply(&mut r, &config);
        assert!(
            r.findings
                .iter()
                .any(|f| f.rule == "short_title" && f.details["classification"] == "advisory")
        );
        assert!(
            !r.findings
                .iter()
                .any(|f| f.rule == "noindex" || f.rule == "missing_hreflang_xdefault")
        );
        assert!(r.pages[0].noindex());
        config.title_min = 1;
        config.require_indexable = true;
        apply(&mut r, &config);
        assert!(!r.findings.iter().any(|f| f.rule == "short_title"));
        assert!(
            r.findings
                .iter()
                .any(|f| f.rule == "noindex" && f.details["classification"] == "policy_violation")
        );
    }
    #[test]
    fn unavailable_page_does_not_create_advisory_and_bad_ranges_fail() {
        let mut r = report();
        r.pages[0].extraction_truncated = true;
        apply(&mut r, &AdvisoryPolicy::default());
        assert!(
            !r.findings
                .iter()
                .any(|f| f.url == r.pages[0].url && f.rule == "little_content")
        );
        let config = AdvisoryPolicy {
            title_min: 100,
            title_max: 5,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}
