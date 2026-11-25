//! Schema-4 `finding_coverage` extension and the production serialiser.
//!
//! The top-level `finding_coverage` extension is a *declaration*, not proof. It
//! states which pinned checks were evaluated completely over the retained
//! evidence and whether the whole pinned population may be called complete.
//! The receiver alone independently verifies the population. Until it has a
//! verifier for every catalogue rule, a producer `audit:complete` is a
//! "claimed, unverified" status and must never be read as clean. An absent or
//! unknown check is never evidence of absence.
//!
//! Two independent dimensions compose each check:
//! * *full recomputation* — the retained findings equal the re-derived pinned
//!   `audit::analyze` + policy population (including multiplicity). A mismatch
//!   is a producer contradiction and fails closed.
//! * *source adequacy* — the retained pages/edges needed by that rule were
//!   fully observed. Item-local rules need each non-resource page observed;
//!   graph/cross-page and external-target rules additionally need a complete
//!   crawl and observed link targets.
//!
//! `audit:complete` requires every catalogue check complete; it does **not**
//! mean the report has no findings (a complete check may contain expected
//! findings). `unknown` is never clean.

use crate::{
    audit,
    model::{Link, Page, Report},
    policy::AdvisoryPolicy,
    policy_wire_prototype,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Wire version of the local `finding_coverage` extension.
pub const FINDING_COVERAGE_VERSION: u64 = 1;

/// Rules whose expected population depends on other retained pages or edges.
/// Every other catalogue rule is treated as item-local. This list is
/// intentionally conservative: a rule is only complete when the retained graph
/// is complete and every required target is observed.
const GRAPH_RULES: &[&str] = &[
    "broken_resource",
    "insecure_resource",
    "external_link_broken",
    "external_link_redirect",
    "external_link",
    "external_target",
    "duplicate_title",
    "duplicate_description",
    "duplicate_content",
    "broken_internal_link",
    "redirect_chain",
    "redirect_loop",
    "orphan_page",
    "mixed_incoming_rel",
    "nofollow_indexable",
    "canonical_to_error",
    "canonical_to_redirect",
    "canonical_to_noindex",
    "canonical_chain",
    "hreflang_return_missing",
    "hreflang_to_error",
    "hreflang_to_redirect",
    "hreflang_to_noindex",
    "hreflang_noncanonical",
    "pagination_not_linked",
];

/// Full pinned audit + policy rule catalogue that a global complete claim must
/// cover. Declaring only one rule (for example `missing_h1`) is a demonstration,
/// not the catalogue.
#[must_use]
pub fn catalog() -> Vec<String> {
    let mut ids: BTreeSet<String> = audit::rules().iter().map(|r| r.id.to_owned()).collect();
    for guidance in crate::policy::catalog() {
        ids.insert(guidance.rule);
    }
    ids.into_iter().collect()
}

/// Strict successful-HTML observation used by item-local checks and as a graph
/// target requirement.
fn page_observable(page: &Page) -> bool {
    !page.is_resource
        && page.status.is_some_and(|s| (200..300).contains(&s))
        && page.html()
        && !page.extraction_truncated
        && !page.blocked_by_robots
        && page.error.is_none()
}

/// Any observed non-resource response (successful HTML or not). A 3xx/4xx page
/// is fully observed even though item-local HTML checks do not apply to it.
fn page_observed(page: &Page) -> bool {
    !page.is_resource && page.status.is_some() && page.error.is_none() && !page.blocked_by_robots
}

/// A retained target page that graph rules may consult must be a non-resource
/// page with an observed successful HTML response. Existence or `is_resource`
/// alone is not enough.
fn page_target_observed(report: &Report, url: &str) -> bool {
    report
        .pages
        .iter()
        .any(|t| t.url == url && page_observable(t))
}

/// A retained resource dependency target must have an observed response.
fn resource_target_observed(report: &Report, url: &str) -> bool {
    report.pages.iter().any(|t| {
        t.url == url
            && t.is_resource
            && t.status.is_some()
            && t.error.is_none()
            && !t.blocked_by_robots
    })
}

/// Item-local prerequisite, never vacuously true: at least one successful HTML
/// non-resource page must be observed and the actual seed URL must be present
/// and usable. Every retained non-resource page must at least be observed
/// (`status` present, no error, not robots-blocked). Only pages that actually
/// parse as 2xx HTML are subject to the truncation test; a truncated 2xx HTML
/// page withholds the complete claim because item-local rules cannot be
/// certified on a partial extraction. Non-HTML and non-2xx responses are fully
/// observed responses that item-local HTML rules do not evaluate, so they
/// satisfy this prerequisite without themselves withholding it.
fn item_sources_adequate(report: &Report) -> bool {
    let mut html_observed = false;
    for page in &report.pages {
        if page.is_resource {
            continue;
        }
        if !page_observed(page) {
            return false;
        }
        if page.html() {
            if page.extraction_truncated {
                return false;
            }
            html_observed = true;
        }
    }
    html_observed
        && report
            .pages
            .iter()
            .any(|p| p.url == report.request.url.as_str() && page_observable(p))
}

/// Graph prerequisite: a complete crawl plus observed targets for every edge,
/// resource relationship, redirect, canonical and alternate the graph rules
/// consult. Missing or ambiguous targets and unresolved redirects stay unknown.
fn graph_sources_adequate(report: &Report) -> bool {
    if !report.coverage_complete
        || report.outcome != "completed"
        || report.stop_reason != "frontier_exhausted"
        || !item_sources_adequate(report)
    {
        return false;
    }
    for page in &report.pages {
        if !page.is_resource && page.html() {
            for link in &page.links {
                let external = url::Url::parse(&link.url)
                    .is_ok_and(|u| u.host_str() != report.request.url.host_str());
                let observed = page_target_observed(report, &link.url);
                if external {
                    if !report.request.check_external_links || !observed {
                        return false;
                    }
                } else if !observed {
                    return false;
                }
            }
        }
        for relationship in &page.relationships {
            if relationship.resource && !resource_target_observed(report, &relationship.url) {
                return false;
            }
        }
        if let Some(target) = page.redirect_target()
            && target != page.url
            && !page_target_observed(report, target)
        {
            return false;
        }
        if let Some(canonical) = &page.canonical
            && canonical != &page.url
            && !page_target_observed(report, canonical)
        {
            return false;
        }
        for alternate in &page.alternates {
            if alternate.url != page.url && !page_target_observed(report, &alternate.url) {
                return false;
            }
        }
    }
    true
}

/// True when **any** pinned policy coverage row for this rule shows an omission
/// (or is otherwise incomplete). A rule can carry several rows: `noindex` and
/// `http_redirect` each have an `observation` row and a `policy_violation` row.
/// Inspecting only the first row can hide a `record_limit` omission in a later
/// row and wrongly certify the rule, so **every** row for the rule must be
/// accounted for. Any omitted candidate for a row forces that row's replaced
/// findings unknown. An absent coverage row is not itself an omission; other
/// source-adequacy gates then decide the check.
fn policy_omitted(report: &Report, rule: &str) -> bool {
    report
        .policy_guidance
        .as_ref()
        .and_then(|guidance| guidance.candidate.as_ref())
        .is_some_and(|candidate| {
            candidate
                .coverage
                .iter()
                .filter(|row| row.rule_id == rule)
                .any(|row| !row.complete || row.omitted > 0)
        })
}

/// A check is complete only when sources are adequate *and* the pinned policy
/// evaluation of that rule omitted no candidate (record/observation caps force
/// the replaced findings to unknown).
fn check_status(report: &Report, rule: &str, item_ok: bool, graph_ok: bool) -> &'static str {
    if policy_omitted(report, rule) {
        return "unknown";
    }
    let adequate = if GRAPH_RULES.contains(&rule) {
        graph_ok
    } else {
        item_ok
    };
    if adequate { "complete" } else { "unknown" }
}

/// Re-derive the full pinned audit + policy finding population (including
/// multiplicity) from the retained pages, applying policy retain/replace/
/// suppress ordering. Full recomputation is independent of source adequacy.
fn expected_findings(report: &Report, policy: &AdvisoryPolicy) -> Result<Vec<Value>> {
    let mut clone = report.clone();
    clone.schema_version = 1;
    clone.policy_guidance = None;
    clone.findings = audit::analyze(&clone.pages, &clone.request.url);
    crate::policy::apply_with_evidence(&mut clone, policy)?;
    clone
        .findings
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, serde_json::Error>>()
        .map_err(Into::into)
}

fn multiset(values: Vec<Value>) -> Vec<String> {
    let mut out: Vec<String> = values.into_iter().map(|v| v.to_string()).collect();
    out.sort_unstable();
    out
}

/// Validate the generic wire v1 extension shape before it is interpreted:
/// bounded by the catalogue, unique and sorted, known status strings, exactly
/// the three agreed members, and no unknown rule ids. A `partial` extension may
/// omit checks (absent means unknown) or list any mix of complete/unknown. A
/// `complete` extension must list every catalogue rule as `complete`. This is
/// the generic contract; the producer additionally emits the full catalogue
/// (see [`validate_emitted_full_catalogue`]). This does not verify the claim
/// against source; the receiver does.
pub fn validate_extension(value: &Value) -> Result<()> {
    let extension = value
        .get("finding_coverage")
        .ok_or_else(|| anyhow::anyhow!("finding_coverage missing"))?;
    let object = extension
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("finding_coverage must be an object"))?;
    ensure!(
        object.len() == 3
            && object.contains_key("version")
            && object.contains_key("audit")
            && object.contains_key("checks"),
        "unexpected finding_coverage members"
    );
    ensure!(
        extension["version"] == json!(FINDING_COVERAGE_VERSION),
        "unsupported finding_coverage version"
    );
    let audit = extension["audit"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("finding_coverage.audit missing"))?;
    ensure!(
        matches!(audit, "complete" | "partial"),
        "invalid audit status"
    );
    let checks = extension["checks"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("finding_coverage.checks missing"))?;
    let catalogue = catalog();
    ensure!(
        checks.len() <= catalogue.len(),
        "check count exceeds the catalogue"
    );
    let mut previous: Option<&str> = None;
    let mut all_complete = true;
    for check in checks {
        let check = check
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("check must be an object"))?;
        ensure!(
            check.len() == 2,
            "unexpected finding_coverage check members"
        );
        let rule_id = check["rule_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("rule_id missing"))?;
        ensure!(
            catalogue.iter().any(|id| id.as_str() == rule_id),
            "unknown rule_id"
        );
        if let Some(previous) = previous {
            ensure!(previous < rule_id, "checks not unique and sorted");
        }
        previous = Some(rule_id);
        let status = check["status"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("status missing"))?;
        ensure!(
            matches!(status, "complete" | "unknown"),
            "invalid check status"
        );
        all_complete &= status == "complete";
    }
    if audit == "complete" {
        ensure!(all_complete, "complete audit lists an unknown check");
        ensure!(
            checks.len() == catalogue.len(),
            "complete audit omits catalogue checks"
        );
    }
    Ok(())
}

/// Producer emission invariant (stricter than the generic wire rules): this
/// generator always emits the full sorted catalogue so the receiver sees every
/// check name. Absence is still allowed on the wire for other emitters.
pub fn validate_emitted_full_catalogue(value: &Value) -> Result<()> {
    let checks = value["finding_coverage"]["checks"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("finding_coverage.checks missing"))?;
    let catalogue = catalog();
    ensure!(
        checks.len() == catalogue.len(),
        "producer must emit the full catalogue"
    );
    ensure!(
        checks.iter().zip(&catalogue).all(|(check, expected)| {
            check["rule_id"].as_str().is_some_and(|id| id == expected)
        }),
        "producer catalogue order or content differs"
    );
    Ok(())
}

/// Deterministic synthetic input for the coverage fixtures. Derived from the
/// local complete policy fixture with a documented mutation recipe: keep only
/// page 0, clear its link/graph fields, then vary headings, truncation and one
/// unobserved external edge. Public so the generator example and the
/// reproducibility test share exactly one recipe.
///
/// # Errors
/// Returns a parse error if the embedded base fixture is malformed.
pub fn fixture_input(case: &str) -> Result<Report> {
    let mut report: Report =
        serde_json::from_str(include_str!("../fixtures/policies/v3-local-complete.json"))?;
    report.schema_version = 1;
    report.policy_guidance = None;
    report.evidence_contract = None;
    report.pages.truncate(1);
    report.pages[0].links.clear();
    report.pages[0].relationships.clear();
    report.pages[0].alternates.clear();
    report.coverage_complete = true;
    report.outcome = "completed".into();
    report.stop_reason = "frontier_exhausted".into();
    report.pages[0].headings = if case.ends_with("clean-h1") {
        vec![(1, "A heading".into())]
    } else {
        vec![]
    };
    match case {
        "partial-missing-h1" => {
            report.pages[0].extraction_truncated = true;
            report.coverage_complete = false;
            report.stop_reason = "page_limit".into();
        }
        "partial-clean-h1" => {
            report.request.check_external_links = false;
            report.pages[0].links = vec![Link {
                url: "https://outside.example/not-fetched".into(),
                ..Default::default()
            }];
        }
        _ => {}
    }
    Ok(report)
}

/// Run the actual audit and policy pipeline before adding the experimental
/// extension. Full recomputation is a fail-closed contradiction check; source
/// adequacy then decides complete versus partial per rule.
pub fn generate(report: &mut Report, policy: &AdvisoryPolicy) -> Result<Vec<u8>> {
    generate_with(report, policy, false)
}

/// Production serialiser: audit, policy, S01 evidence over the final findings,
/// then `finding_coverage`, serialised last. The bytes are stored and delivered
/// unchanged.
pub fn generate_wire_v1(report: &mut Report, policy: &AdvisoryPolicy) -> Result<Vec<u8>> {
    generate_with(report, policy, true)
}

fn generate_with(report: &mut Report, policy: &AdvisoryPolicy, evidence: bool) -> Result<Vec<u8>> {
    ensure!(
        report.schema_version == 1 && report.policy_guidance.is_none(),
        "base report required"
    );
    report.findings = audit::analyze(&report.pages, &report.request.url);
    let bytes = if evidence {
        policy_wire_prototype::generate_wire_v1(report, policy)?
    } else {
        policy_wire_prototype::generate(report, policy)?
    };
    let expected = expected_findings(report, policy)?;
    let retained = report
        .findings
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    ensure!(
        multiset(retained) == multiset(expected),
        "retained findings differ from the pinned population"
    );
    let item_ok = item_sources_adequate(report);
    let graph_ok = graph_sources_adequate(report);
    let catalogue = catalog();
    let statuses: Vec<&str> = catalogue
        .iter()
        .map(|rule| check_status(report, rule, item_ok, graph_ok))
        .collect();
    let all_complete = statuses.iter().all(|status| *status == "complete");
    let checks = Value::Array(
        catalogue
            .into_iter()
            .zip(statuses)
            .map(|(rule_id, status)| json!({"rule_id": rule_id, "status": status}))
            .collect(),
    );
    let mut value: Value = serde_json::from_slice(&bytes)?;
    value["finding_coverage"] = json!({
        "version": FINDING_COVERAGE_VERSION,
        "audit": if all_complete { "complete" } else { "partial" },
        "checks": checks,
    });
    validate_extension(&value)?;
    validate_emitted_full_catalogue(&value)?;
    let bytes = serde_json::to_vec(&value)?;
    ensure!(
        bytes.len() <= crate::delivery::HTP_MAX_REPORT_BYTES,
        "prototype exceeds report byte limit"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_value(case: &str) -> Value {
        let mut report = fixture_input(case).unwrap();
        serde_json::from_slice(&generate(&mut report, &AdvisoryPolicy::default()).unwrap()).unwrap()
    }

    fn status(value: &Value, rule: &str) -> String {
        value["finding_coverage"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["rule_id"] == rule)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn catalogue_is_the_full_pinned_population_not_one_rule() {
        let catalogue = catalog();
        assert!(
            catalogue.windows(2).all(|w| w[0] < w[1]),
            "not unique sorted"
        );
        assert!(catalogue.len() > 50, "catalogue suspiciously small");
        for rule in [
            "missing_h1",
            "missing_title",
            "invalid_language",
            "orphan_page",
            "hreflang_return_missing",
            "pagination_not_linked",
            "alternates_observed",
            "external_link",
        ] {
            assert!(catalogue.iter().any(|id| id == rule), "missing {rule}");
        }
    }

    #[test]
    fn complete_audit_may_still_contain_an_observed_finding() {
        let missing = generate_value("complete-missing-h1");
        assert!(
            missing["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["rule"] == "missing_h1")
        );
        assert_eq!(missing["finding_coverage"]["audit"], "complete");
        assert!(
            missing["finding_coverage"]["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["status"] == "complete")
        );

        let clean = generate_value("complete-clean-h1");
        assert!(
            !clean["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["rule"] == "missing_h1")
        );
        assert_eq!(clean["finding_coverage"]["audit"], "complete");
        assert_eq!(status(&clean, "missing_h1"), "complete");
        // Complete is not globally clean: other expected findings remain.
        assert!(
            clean["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["rule"] == "little_content")
        );
    }

    #[test]
    fn partial_honours_unknown_and_a_verified_clean_item_check() {
        let unknown = generate_value("partial-missing-h1");
        assert_eq!(unknown["finding_coverage"]["audit"], "partial");
        assert!(
            unknown["finding_coverage"]["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["status"] == "unknown")
        );
        // The observed finding stays visible while coverage is unknown.
        assert!(
            unknown["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["rule"] == "missing_h1")
        );

        let partial = generate_value("partial-clean-h1");
        assert_eq!(partial["finding_coverage"]["audit"], "partial");
        assert!(
            !partial["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["rule"] == "missing_h1")
        );
        // An item-local check may be complete and clean while graph checks stay
        // unknown and the global audit stays partial.
        assert_eq!(status(&partial, "missing_h1"), "complete");
        assert_eq!(status(&partial, "little_content"), "complete");
        assert_eq!(status(&partial, "external_link"), "unknown");
        assert_eq!(status(&partial, "orphan_page"), "unknown");
    }

    #[test]
    fn empty_all_resource_and_unobserved_evidence_withhold_complete() {
        // Empty retained evidence is not vacuous truth.
        let mut empty = fixture_input("complete-missing-h1").unwrap();
        empty.pages.clear();
        assert!(!item_sources_adequate(&empty));
        assert!(!graph_sources_adequate(&empty));

        // All-resource evidence has no observable HTML page.
        let mut resources = fixture_input("complete-missing-h1").unwrap();
        resources.pages[0].is_resource = true;
        assert!(!item_sources_adequate(&resources));
        assert!(!graph_sources_adequate(&resources));

        // A usable seed at the request URL is required.
        let mut wrong_seed = fixture_input("complete-missing-h1").unwrap();
        wrong_seed.pages[0].url = "https://example.com/other".into();
        assert!(!item_sources_adequate(&wrong_seed));

        // An unobserved external target stays unknown with checking disabled...
        let mut external = fixture_input("partial-clean-h1").unwrap();
        assert!(item_sources_adequate(&external));
        assert!(!graph_sources_adequate(&external));

        // ...and with checking enabled.
        external.request.check_external_links = true;
        assert!(!graph_sources_adequate(&external));

        // Mere target existence with no status is not a usable observation, and
        // a non-resource link target flagged as a resource does not qualify.
        external.pages.push(Page {
            url: "https://outside.example/not-fetched".into(),
            is_resource: true,
            ..Default::default()
        });
        assert!(!graph_sources_adequate(&external));

        // An unresolved redirect keeps graph coverage unknown while the seed
        // itself stays observable.
        let mut redirect = fixture_input("complete-missing-h1").unwrap();
        redirect.pages.push(Page {
            url: "https://example.com/moved".into(),
            status: Some(301),
            content_type: "text/html".into(),
            redirect: Some("https://example.com/final".into()),
            ..Default::default()
        });
        assert!(item_sources_adequate(&redirect));
        assert!(!graph_sources_adequate(&redirect));
    }

    #[test]
    fn every_policy_row_for_a_rule_is_accounted_for() {
        // `noindex` (like `http_redirect`) carries two coverage rows: an
        // `observation` row and a `policy_violation` row. Reproduce the F1
        // mixed status where the earlier observation row is complete while the
        // later policy_violation row omitted candidates under the record cap.
        // The old `.find` logic certified the rule from the first row alone.
        let mut report = fixture_input("complete-clean-h1").unwrap();
        report.findings = audit::analyze(&report.pages, &report.request.url);
        policy_wire_prototype::generate(&mut report, &AdvisoryPolicy::default()).unwrap();

        let rows = report
            .policy_guidance
            .as_ref()
            .unwrap()
            .candidate
            .as_ref()
            .unwrap()
            .coverage
            .iter()
            .filter(|row| row.rule_id == "noindex")
            .count();
        assert!(rows >= 2, "expected multiple policy rows for noindex");
        assert!(item_sources_adequate(&report));
        assert_eq!(check_status(&report, "noindex", true, true), "complete");

        let candidate = report
            .policy_guidance
            .as_mut()
            .unwrap()
            .candidate
            .as_mut()
            .unwrap();
        assert!(
            candidate
                .coverage
                .iter()
                .find(|row| row.rule_id == "noindex" && row.category == "observation")
                .unwrap()
                .complete,
            "observation row should stay complete"
        );
        for row in &mut candidate.coverage {
            if row.rule_id == "noindex" && row.category == "policy_violation" {
                row.omitted = 1;
                row.complete = false;
            }
        }
        // Only iterating every row sees the omitted later row.
        assert_eq!(check_status(&report, "noindex", true, true), "unknown");
        // An unrelated rule whose rows are all complete is unaffected.
        assert_eq!(
            check_status(&report, "http_redirect", true, true),
            "complete"
        );
    }

    #[test]
    fn remaining_source_adequacy_edges_are_explicit() {
        // A fully observed 2xx non-HTML page does not withhold item-local
        // coverage: HTML item rules do not evaluate it.
        let mut non_html = fixture_input("complete-missing-h1").unwrap();
        non_html.pages.push(Page {
            url: "https://example.com/data.json".into(),
            status: Some(200),
            content_type: "application/json".into(),
            ..Default::default()
        });
        assert!(page_observed(&non_html.pages[1]));
        assert!(!non_html.pages[1].html());
        assert!(item_sources_adequate(&non_html));
        assert!(graph_sources_adequate(&non_html));

        // A non-2xx HTML response is observed too and also does not withhold
        // item-local coverage (`Page::html` is 2xx-only, so the truncation test
        // does not apply).
        let mut not_found = fixture_input("complete-missing-h1").unwrap();
        not_found.pages.push(Page {
            url: "https://example.com/gone".into(),
            status: Some(404),
            content_type: "text/html".into(),
            ..Default::default()
        });
        assert!(page_observed(&not_found.pages[1]));
        assert!(!not_found.pages[1].html());
        assert!(item_sources_adequate(&not_found));

        // A truncated 2xx HTML page withholds item-local coverage.
        let mut truncated = fixture_input("complete-missing-h1").unwrap();
        truncated.pages.push(Page {
            url: "https://example.com/partial".into(),
            status: Some(200),
            content_type: "text/html".into(),
            extraction_truncated: true,
            ..Default::default()
        });
        assert!(!item_sources_adequate(&truncated));

        // Graph prerequisites are independent of item adequacy: an incomplete
        // crawl, a non-`frontier_exhausted` stop or a non-`completed` outcome
        // withholds graph checks even when every item-local source is observed.
        let mut incomplete = fixture_input("complete-missing-h1").unwrap();
        incomplete.coverage_complete = false;
        assert!(item_sources_adequate(&incomplete));
        assert!(!graph_sources_adequate(&incomplete));

        let mut not_exhausted = fixture_input("complete-missing-h1").unwrap();
        not_exhausted.stop_reason = "max_pages".into();
        assert!(!graph_sources_adequate(&not_exhausted));

        let mut not_completed = fixture_input("complete-missing-h1").unwrap();
        not_completed.outcome = "aborted".into();
        assert!(!graph_sources_adequate(&not_completed));

        // An unobserved internal link target withholds graph checks.
        let mut internal = fixture_input("complete-missing-h1").unwrap();
        internal.pages[0].links = vec![Link {
            url: "https://example.com/internal-missing".into(),
            ..Default::default()
        }];
        assert!(item_sources_adequate(&internal));
        assert!(!graph_sources_adequate(&internal));

        // A fully observed extra page resolves an internal target.
        let mut multipage = fixture_input("complete-missing-h1").unwrap();
        multipage.pages.push(Page {
            url: "https://example.com/child".into(),
            status: Some(200),
            content_type: "text/html".into(),
            ..Default::default()
        });
        multipage.pages[0].links = vec![Link {
            url: "https://example.com/child".into(),
            ..Default::default()
        }];
        assert!(item_sources_adequate(&multipage));
        assert!(graph_sources_adequate(&multipage));

        // `is_resource` evidence is never item-adequate on its own.
        let mut resource_only = fixture_input("complete-missing-h1").unwrap();
        resource_only.pages[0].is_resource = true;
        assert!(!item_sources_adequate(&resource_only));
    }

    #[test]
    fn extension_rejects_wrong_catalogue_or_status() {
        let mut value = generate_value("complete-missing-h1");
        value["finding_coverage"]["checks"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(validate_extension(&value).is_err());

        let mut value = generate_value("complete-missing-h1");
        value["finding_coverage"]["checks"][0]["status"] = json!("complete ");
        assert!(validate_extension(&value).is_err());

        let mut value = generate_value("complete-missing-h1");
        value["finding_coverage"]["audit"] = json!("complete");
        value["finding_coverage"]["checks"][0]["status"] = json!("unknown");
        assert!(validate_extension(&value).is_err());

        let mut value = generate_value("complete-missing-h1");
        value["finding_coverage"]["version"] = json!(2);
        assert!(validate_extension(&value).is_err());

        let mut value = generate_value("complete-missing-h1");
        value["finding_coverage"]["extra"] = json!(true);
        assert!(validate_extension(&value).is_err());
    }

    #[test]
    fn generic_partial_wire_allows_subsets_but_emitter_is_full() {
        let mut value = generate_value("partial-clean-h1");
        // Generic wire: a partial claim may omit checks (absent = unknown).
        let mut subset = value.clone();
        subset["finding_coverage"]["checks"] =
            json!([{"rule_id": "missing_h1", "status": "complete"}]);
        assert!(validate_extension(&subset).is_ok());
        // The producer emission invariant always lists the full catalogue.
        assert!(validate_emitted_full_catalogue(&subset).is_err());

        // Generic wire: a conservatively partial claim may list only complete
        // checks and still be partial without any listed unknown.
        let mut all_known = value.clone();
        for check in all_known["finding_coverage"]["checks"]
            .as_array_mut()
            .unwrap()
        {
            check["status"] = json!("complete");
        }
        assert!(validate_extension(&all_known).is_ok());

        // Unknown rule ids, duplicates and unsorted ids are rejected.
        let mut unknown = value.clone();
        unknown["finding_coverage"]["checks"][0]["rule_id"] = json!("not_a_rule");
        assert!(validate_extension(&unknown).is_err());

        let mut duplicate = value.clone();
        let first = duplicate["finding_coverage"]["checks"][0].clone();
        duplicate["finding_coverage"]["checks"]
            .as_array_mut()
            .unwrap()
            .push(first);
        assert!(validate_extension(&duplicate).is_err());

        value["finding_coverage"]["checks"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert!(validate_extension(&value).is_err());
    }

    #[test]
    fn population_contradiction_fails_closed() {
        let report = fixture_input("complete-missing-h1").unwrap();
        let expected = expected_findings(&report, &AdvisoryPolicy::default()).unwrap();
        assert!(
            expected.iter().any(|f| f["rule"] == "missing_h1"),
            "expected population lacks missing_h1"
        );
        let mut missing = expected.clone();
        missing.pop();
        assert_ne!(
            multiset(missing),
            multiset(expected_findings(&report, &AdvisoryPolicy::default()).unwrap())
        );
        let mut duplicated = expected.clone();
        let first = duplicated[0].clone();
        duplicated.push(first);
        assert_ne!(
            multiset(duplicated),
            multiset(expected_findings(&report, &AdvisoryPolicy::default()).unwrap())
        );
    }

    #[test]
    fn committed_fixtures_are_reproducible() {
        for (case, fixture) in [
            (
                "complete-missing-h1",
                include_str!(
                    "../fixtures/contracts/s14-finding-coverage-v1/complete-missing-h1.json"
                ),
            ),
            (
                "complete-clean-h1",
                include_str!(
                    "../fixtures/contracts/s14-finding-coverage-v1/complete-clean-h1.json"
                ),
            ),
            (
                "partial-missing-h1",
                include_str!(
                    "../fixtures/contracts/s14-finding-coverage-v1/partial-missing-h1.json"
                ),
            ),
            (
                "partial-clean-h1",
                include_str!("../fixtures/contracts/s14-finding-coverage-v1/partial-clean-h1.json"),
            ),
        ] {
            let mut report = fixture_input(case).unwrap();
            let bytes = generate(&mut report, &AdvisoryPolicy::default()).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                serde_json::from_str::<Value>(fixture).unwrap(),
                "{case} fixture differs"
            );
            assert!(bytes == fixture.as_bytes(), "{case} bytes differ");
        }
    }
}
