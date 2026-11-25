use seosmith::{
    audit,
    contract::{self, Outcome, entity_id},
    model::{CrawlRequest, Page, Report, digest},
    policy::{self, AdvisoryPolicy},
};
use std::collections::HashSet;

fn page(url: &str, title: &str) -> Page {
    Page {
        url: url.into(),
        status: Some(200),
        content_type: "text/html".into(),
        encoding: "utf-8".into(),
        title: title.into(),
        ..Page::default()
    }
}

/// Builds a report and applies the advisory policy, exactly as the
/// producer does before it calls `attach_v1_schema4`.
fn policy_report() -> Report {
    let request: CrawlRequest = serde_json::from_value(serde_json::json!({
        "request_id":"00000000-0000-4000-8000-000000000001",
        "website_id":"00000000-0000-4000-8000-000000000002",
        "binding_generation":1, "url":"https://example.com/"
    }))
    .unwrap();
    let pages = vec![
        page("https://example.com/", "An observed nonempty title"),
        page("https://example.com/empty", ""),
        page("https://example.com/short", "Hi"),
        Page {
            url: "https://example.com/blocked".into(),
            blocked_by_robots: true,
            ..Page::default()
        },
    ];
    let findings = audit::analyze(&pages, &request.url);
    let mut report = Report {
        schema_version: 1,
        engine_version: "0.2.0".into(),
        ruleset_version: seosmith::model::RULESET_VERSION.into(),
        request,
        started_at: "2026-11-16T12:00:00Z".into(),
        finished_at: "2026-11-16T12:01:00Z".into(),
        outcome: "completed".into(),
        stop_reason: "frontier_exhausted".into(),
        coverage_complete: true,
        pages,
        findings,
        evidence_contract: None,
        policy_guidance: None,
    };
    policy::apply_with_evidence(&mut report, &AdvisoryPolicy::default()).unwrap();
    report
}

#[test]
fn attach_requires_policy_awaiting_schema_4_and_no_evidence() {
    let mut r = policy_report();
    r.policy_guidance = None;
    assert!(contract::attach_v1_schema4(&mut r).is_err());

    for version in [1, 2, 4] {
        let mut r = policy_report();
        r.schema_version = version;
        assert!(contract::attach_v1_schema4(&mut r).is_err());
    }

    let mut r = policy_report();
    contract::attach_v1_schema4(&mut r).unwrap();
    assert!(contract::attach_v1_schema4(&mut r).is_err());
}

#[test]
fn attach_never_sets_schema_version_and_enable_v2_still_does() {
    let mut r = policy_report();
    contract::attach_v1_schema4(&mut r).unwrap();
    assert_eq!(r.schema_version, 3);
    assert_eq!(r.evidence_contract.as_ref().unwrap().version, 1);

    let mut legacy = policy_report();
    legacy.policy_guidance = None;
    legacy.schema_version = 1;
    contract::enable_v2(&mut legacy).unwrap();
    assert_eq!(legacy.schema_version, 2);
}

#[test]
fn evidence_describes_final_delivered_findings() {
    let mut r = policy_report();
    let legacy = audit::analyze(&r.pages, &r.request.url);
    assert!(
        legacy.iter().any(|f| f.rule == "short_title"),
        "fixture must exercise a replaced rule"
    );
    contract::attach_v1_schema4(&mut r).unwrap();
    let json = serde_json::to_value(&r).unwrap();
    let ec = r.evidence_contract.as_ref().unwrap();

    // ID equality: every pointer resolves to the delivered finding with that key.
    let mut failed_keys = HashSet::new();
    for e in &ec.evaluations {
        if let Some(key) = &e.finding_key {
            let j: usize = e
                .source_locator
                .strip_prefix("/findings/")
                .unwrap()
                .parse()
                .unwrap();
            let f = &r.findings[j];
            assert_eq!(&f.key, key);
            assert_eq!(f.rule, e.rule);
            assert_eq!(e.entity_id, entity_id(&f.url, false));
            assert!(json.pointer(&e.source_locator).is_some());
            failed_keys.insert(key.clone());
        }
    }

    // The replaced rule points at the policy finding, in the S14 key namespace.
    let policy_key = digest(b"seosmith-s14-finding-v1\nshort_title\nhttps://example.com/short");
    assert!(failed_keys.contains(&policy_key));
    let legacy_key = legacy
        .iter()
        .find(|f| f.rule == "short_title")
        .unwrap()
        .key
        .clone();
    assert_ne!(legacy_key, policy_key);
    assert!(!failed_keys.contains(&legacy_key));

    // Every delivered finding of an instrumented rule on an observed page has a slot.
    let rules: HashSet<_> = audit::rules().into_iter().map(|r| r.id).collect();
    for f in &r.findings {
        if rules.contains(f.rule.as_str()) {
            assert!(failed_keys.contains(&f.key), "no evaluation for {}", f.key);
        }
    }
}

#[test]
fn rule_removed_by_policy_without_replacement_is_unknown_never_pass() {
    let mut r = policy_report();
    contract::attach_v1_schema4(&mut r).unwrap();
    let ec = r.evidence_contract.as_ref().unwrap();
    // `/` has a fine title: policy emits no short_title there and the legacy
    // population is gone, so nothing may claim a pass.
    let id = entity_id("https://example.com/", false);
    assert!(
        ec.evaluations
            .iter()
            .filter(|e| e.entity_id == id && e.rule != "missing_title")
            .all(|e| e.outcome != Outcome::Pass)
    );
    let short = ec
        .coverage
        .iter()
        .find(|c| c.rule == "short_title")
        .unwrap();
    assert_eq!(short.passed, 0);
    assert!(short.unknown > 0);
}

#[test]
fn policy_only_rules_get_no_evaluation() {
    let mut r = policy_report();
    contract::attach_v1_schema4(&mut r).unwrap();
    let ec = r.evidence_contract.as_ref().unwrap();
    let rules: HashSet<_> = audit::rules().into_iter().map(|r| r.id).collect();
    for name in ["alternates_observed", "external_target", "external_link"] {
        assert!(!rules.contains(name));
        assert!(ec.evaluations.iter().all(|e| e.rule != name));
        assert!(ec.coverage.iter().all(|c| c.rule != name));
    }
}

#[test]
fn attach_does_not_touch_findings_or_policy() {
    let before = policy_report();
    let mut after = before.clone();
    contract::attach_v1_schema4(&mut after).unwrap();
    assert_eq!(
        serde_json::to_value(&before.findings).unwrap(),
        serde_json::to_value(&after.findings).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&before.policy_guidance).unwrap(),
        serde_json::to_value(&after.policy_guidance).unwrap()
    );
}
