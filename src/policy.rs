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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<crate::policy_candidate::Candidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Vec<crate::policy_candidate::ManifestRow>>,
}

pub fn catalog() -> Vec<Guidance> {
    audit::rules().into_iter().filter_map(|r| {
        let (kind, applicability, explanation, evidence, source, fix) = match r.id {
            "short_title" | "long_title" => ("advisory", "Successfully extracted HTML with a nonempty title", "Character count crosses a configured review threshold; display depends on context and there is no universal ideal length.", "page.title; Unicode scalar count", "https://developers.google.com/search/docs/appearance/title-link", "Review whether the title describes the page clearly; edit only where useful."),
            "short_description" | "long_description" => ("advisory", "Successfully extracted HTML with a nonempty description", "Character count crosses a configured review threshold; search snippets vary by query and display.", "page.description; Unicode scalar count", "https://developers.google.com/search/docs/appearance/snippet", "Review whether the description accurately summarizes the page."),
            "little_content" => ("advisory", "Successfully extracted HTML page", "Extracted word count falls below a configured review threshold; word count alone does not determine content quality.", "page.words; extracted word count", "https://developers.google.com/search/docs/fundamentals/creating-helpful-content", "Assess whether the page answers its users' needs; do not add words solely to meet a count."),
            "too_many_links" => ("advisory", "Successfully extracted HTML page", "Outgoing link count exceeds a configured review threshold; this is not a universal link limit.", "page.links; extracted link count", "https://developers.google.com/search/docs/crawling-indexing/links-crawlable", "Review whether links are useful, contextual and crawlable."),
            "noindex" => ("observation_or_policy_violation", "Successfully fetched in-scope HTML page; violation requires explicit indexability intent", "A noindex directive is an observation and may be intentional.", "page.robots; observed directive", "https://developers.google.com/search/docs/crawling-indexing/block-indexing", "If this page should be indexed, review its robots directives."),
            "http_redirect" => ("observation_or_policy_violation", "Successfully fetched in-scope page with an HTTP status; violation requires explicit redirect prohibition", "A redirect is an observation and may be intentional.", "page.status; observed HTTP status", "https://developers.google.com/search/docs/crawling-indexing/301-redirects", "Review the redirect only when it conflicts with the intended URL policy."),
            _ => return None,
        };
        Some(Guidance { rule: r.id.into(), version: "s14-1".into(), kind: kind.into(), applicability: applicability.into(), explanation: explanation.into(), evidence_schema: evidence.into(), source: source.into(), reviewed_at: "2026-11-16".into(), confidence: "observed_data_only".into(), fix_guidance: fix.into() })
    }).chain([
        Guidance { rule: "alternates_observed".into(), version: "s14-1".into(), kind: "observation".into(), applicability: "Successfully extracted HTML page".into(), explanation: "Observed alternate collection; an empty collection is not a defect.".into(), evidence_schema: "page.alternates".into(), source: "https://developers.google.com/search/docs/specialty/international/localized-versions".into(), reviewed_at: "2026-11-16".into(), confidence: "observed_data_only".into(), fix_guidance: "Review alternate language references when relevant.".into() },
        Guidance { rule: "external_target".into(), version: "s14-1".into(), kind: "observation".into(), applicability: "External target recorded as a page".into(), explanation: "A fetch status is an observation; unavailable is unknown.".into(), evidence_schema: "page.status and fetch state".into(), source: "https://developers.google.com/search/docs/crawling-indexing/links-crawlable".into(), reviewed_at: "2026-11-16".into(), confidence: "observed_data_only".into(), fix_guidance: "Inspect the target only when the status conflicts with its intended destination.".into() },
        Guidance { rule: "external_link".into(), version: "s14-1".into(), kind: "observation".into(), applicability: "External link from an in-scope page".into(), explanation: "A referenced target may be unavailable or outside the crawl; this is not a defect by itself.".into(), evidence_schema: "page.links and target status when fetched".into(), source: "https://developers.google.com/search/docs/crawling-indexing/links-crawlable".into(), reviewed_at: "2026-11-16".into(), confidence: "observed_data_only".into(), fix_guidance: "Inspect the link in context if its destination needs review.".into() }
    ]).collect()
}

fn add(findings: &mut Vec<Finding>, page: &Page, rule: &str, evidence: String) {
    findings.push(Finding {
        key: digest(format!("seosmith-s14-finding-v1\n{}\n{}", rule, page.url).as_bytes()),
        rule: rule.into(),
        severity: Severity::Warning,
        url: page.url.clone(),
        evidence_items: vec![evidence.clone()],
        evidence,
        details: serde_json::json!({"classification":"advisory"}),
    });
}
fn apply(report: &mut Report, policy: &AdvisoryPolicy) {
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
    report.findings.retain(|f| {
        if affected.contains(&f.rule.as_str())
            || matches!(
                f.rule.as_str(),
                "external_link_broken"
                    | "external_link_redirect"
                    | "sitemap_noindex"
                    | "canonical_to_noindex"
                    | "hreflang_to_noindex"
                    | "canonical_to_redirect"
                    | "hreflang_to_redirect"
            )
        {
            return false;
        }
        if matches!(
            f.rule.as_str(),
            "fetch_error"
                | "http_client_error"
                | "http_server_error"
                | "robots_blocked"
                | "sitemap_blocked"
        ) && url::Url::parse(&f.url).is_ok_and(|u| !report.request.in_scope(&u))
        {
            return false;
        }
        true
    });
    for p in &report.pages {
        let scoped =
            url::Url::parse(&p.url).is_ok_and(|u| report.request.in_scope(&u)) && !p.is_resource;
        if scoped
            && p.html()
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
        if scoped
            && !crate::policy_candidate::source_unavailable(p)
            && policy.forbid_redirects
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
        candidate: None,
        manifest: None,
    });
}

/// Generate local experimental metadata; no schema 3 payload is queued for delivery.
pub fn apply_with_evidence(report: &mut Report, policy: &AdvisoryPolicy) -> anyhow::Result<()> {
    let (candidate, manifest) = crate::policy_candidate::generate(report, policy)?;
    apply(report, policy);
    if let Some(guidance) = &mut report.policy_guidance {
        guidance.candidate = Some(candidate);
        guidance.manifest = Some(manifest);
    }
    anyhow::ensure!(
        crate::contract::serialized_size(report)? <= crate::contract::MAX_REPORT_BYTES,
        "policy report exceeds delivery size limit"
    );
    Ok(())
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
    fn external_pages_do_not_gain_policy_findings_or_reuse_legacy_keys() {
        let mut r = report();
        let external = crate::model::Page {
            url: "https://outside.example/a".into(),
            status: Some(301),
            content_type: "text/html".into(),
            title: "short".into(),
            ..Default::default()
        };
        let local = &mut r.pages[0];
        local.status = Some(200);
        local.content_type = "text/html".into();
        local.title = "short".into();
        local.error = None;
        local.blocked_by_robots = false;
        local.extraction_truncated = false;
        r.pages.push(external);
        let policy = AdvisoryPolicy {
            forbid_redirects: true,
            ..Default::default()
        };
        apply(&mut r, &policy);
        assert!(
            !r.findings
                .iter()
                .any(|f| f.url == "https://outside.example/a"
                    && (f.rule == "http_redirect" || f.rule == "short_title"))
        );
        let finding = r
            .findings
            .iter()
            .find(|f| f.rule == "short_title" && f.url == r.pages[0].url)
            .unwrap();
        assert_ne!(
            finding.key,
            digest(format!("short_title\n{}", finding.url).as_bytes())
        );
    }
    #[test]
    fn blocked_external_target_is_unknown_without_legacy_warning() {
        let mut report = report();
        report.request.check_external_links = true;
        let target = crate::model::Page {
            url: "https://outside.example/blocked".into(),
            is_resource: true,
            blocked_by_robots: true,
            ..Default::default()
        };
        report.pages.push(target);
        report.findings = crate::audit::analyze(&report.pages, &report.request.url);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.url == "https://outside.example/blocked" && f.rule == "robots_blocked")
        );
        apply_with_evidence(&mut report, &AdvisoryPolicy::default()).unwrap();
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.url == "https://outside.example/blocked"
                    && (f.rule == "robots_blocked" || f.rule == "sitemap_blocked"))
        );
        let candidate = report
            .policy_guidance
            .as_ref()
            .unwrap()
            .candidate
            .as_ref()
            .unwrap();
        let entity = crate::contract::entity_id("https://outside.example/blocked", true);
        assert!(
            candidate
                .records
                .iter()
                .any(|r| r.rule_id == "external_target"
                    && r.entity_id == entity
                    && r.outcome == "unknown")
        );
    }
    #[test]
    fn redirect_requires_explicit_intent_and_observed_status() {
        let mut r = report();
        r.pages[0].status = Some(301);
        let mut config = AdvisoryPolicy::default();
        apply(&mut r, &config);
        assert!(!r.findings.iter().any(|f| f.rule == "http_redirect"));
        config.forbid_redirects = true;
        apply(&mut r, &config);
        assert!(r.findings.iter().any(
            |f| f.rule == "http_redirect" && f.details["classification"] == "policy_violation"
        ));
        r.pages[0].status = None;
        apply(&mut r, &config);
        assert!(!r.findings.iter().any(|f| f.rule == "http_redirect"));
    }
    #[test]
    fn redirect_findings_require_matching_completed_violation() {
        let mut report = report();
        report.pages.clear();
        for (i, condition) in ["available", "truncated", "error", "blocked"]
            .iter()
            .enumerate()
        {
            let mut page = crate::model::Page {
                url: format!("https://example.com/{i}"),
                status: Some(301),
                ..Default::default()
            };
            match *condition {
                "truncated" => page.extraction_truncated = true,
                "error" => page.error = Some("fetch_failed".into()),
                "blocked" => page.blocked_by_robots = true,
                _ => {}
            }
            report.pages.push(page);
        }
        let config = AdvisoryPolicy {
            forbid_redirects: true,
            ..Default::default()
        };
        apply_with_evidence(&mut report, &config).unwrap();
        let candidate = report
            .policy_guidance
            .as_ref()
            .unwrap()
            .candidate
            .as_ref()
            .unwrap();
        for page in &report.pages {
            let id = crate::contract::entity_id(&page.url, page.is_resource);
            let record = candidate
                .records
                .iter()
                .find(|r| {
                    r.category == "policy_violation"
                        && r.rule_id == "http_redirect"
                        && r.entity_id == id
                })
                .unwrap();
            let finding = report.findings.iter().any(|f| {
                f.url == page.url
                    && f.rule == "http_redirect"
                    && f.details["classification"] == "policy_violation"
            });
            assert_eq!(finding, record.outcome == "violation");
            if crate::policy_candidate::source_unavailable(page) {
                assert_eq!(record.outcome, "unknown");
            }
        }
    }
    #[test]
    fn catalog_documents_only_reviewed_rules() {
        let catalog = catalog();
        assert!(
            catalog.iter().any(
                |g| g.rule == "little_content" && g.source.contains("creating-helpful-content")
            )
        );
        assert!(
            catalog
                .iter()
                .any(|g| g.rule == "alternates_observed" && g.kind == "observation")
        );
        assert!(!catalog.iter().any(|g| g.rule == "missing_title"));
        assert!(
            !catalog
                .iter()
                .any(|g| g.rule == "missing_hreflang_xdefault")
        );
        assert!(
            catalog
                .iter()
                .find(|g| g.rule == "noindex")
                .unwrap()
                .applicability
                .contains("Successfully fetched")
        );
        assert!(
            catalog
                .iter()
                .find(|g| g.rule == "http_redirect")
                .unwrap()
                .applicability
                .contains("HTTP status")
        );
        assert!(catalog.iter().all(|g| !g.applicability.is_empty()
            && !g.evidence_schema.is_empty()
            && !g.fix_guidance.is_empty()));
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
