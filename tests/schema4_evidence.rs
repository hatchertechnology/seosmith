use seosmith::{
    audit,
    contract::{self, Outcome, entity_id},
    model::{CrawlRequest, Page, Report, digest},
    policy::{self, AdvisoryPolicy},
};
use std::collections::{HashMap, HashSet};

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
    policy_report_with(vec![
        page("https://example.com/", "An observed nonempty title"),
        page("https://example.com/empty", ""),
        page("https://example.com/short", "Hi"),
        Page {
            url: "https://example.com/blocked".into(),
            blocked_by_robots: true,
            ..Page::default()
        },
        // 200 HTML, empty title, extraction hit a cap: a delivered
        // missing_title finding must still get an evaluation slot.
        Page {
            extraction_truncated: true,
            ..page("https://example.com/trunc", "")
        },
    ])
}

fn policy_report_with(pages: Vec<Page>) -> Report {
    let request: CrawlRequest = serde_json::from_value(serde_json::json!({
        "request_id":"00000000-0000-4000-8000-000000000001",
        "website_id":"00000000-0000-4000-8000-000000000002",
        "binding_generation":1, "url":"https://example.com/"
    }))
    .unwrap();
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

/// §5 ID-equality test over the re-parsed serialized bytes. Returns
/// (excluded-by-cap count, number of fail evaluations).
fn assert_id_equality(r: &Report) -> (usize, usize) {
    let bytes = serde_json::to_vec(r).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let findings = doc["findings"].as_array().unwrap();
    let evals = doc["evidence_contract"]["evaluations"].as_array().unwrap();
    let instrumented: HashSet<&str> = ["missing_title"].into();
    let rules: Vec<&str> = audit::rules().into_iter().map(|r| r.id).collect();

    // K_ev: multiset of finding_key over fail evaluations; each must carry a key.
    let mut k_ev: Vec<String> = Vec::new();
    for e in evals {
        let outcome = e["outcome"].as_str().unwrap();
        if outcome == "fail" {
            let key = e["finding_key"].as_str().expect("fail needs finding_key");
            let n: usize = e["source_locator"]
                .as_str()
                .unwrap()
                .strip_prefix("/findings/")
                .expect("fail points at a finding")
                .parse()
                .unwrap();
            assert_eq!(findings[n]["key"], key);
            assert_eq!(findings[n]["rule"], e["rule"]);
            assert!(doc.pointer(e["source_locator"].as_str().unwrap()).is_some());
            k_ev.push(key.to_owned());
        } else {
            assert!(
                e["finding_key"].is_null(),
                "only fail carries a finding_key"
            );
        }
        if outcome == "pass" {
            assert!(instrumented.contains(e["rule"].as_str().unwrap()));
        }
    }

    // K_del: delivered findings of audit rules whose (rule, URL) got a slot,
    // simulating the MAX_OBSERVATIONS / MAX_EVALUATIONS order independently.
    let mut by_rule_url: HashMap<(&str, &str), &str> = HashMap::new();
    for f in findings {
        let (rule, url) = (f["rule"].as_str().unwrap(), f["url"].as_str().unwrap());
        if rules.contains(&rule) {
            assert!(
                by_rule_url
                    .insert((rule, url), f["key"].as_str().unwrap())
                    .is_none()
            );
        }
    }
    let mut k_del: Vec<String> = Vec::new();
    let mut excluded = 0;
    let mut slots = 0;
    for rule in &rules {
        for (i, p) in r.pages.iter().enumerate() {
            let found = by_rule_url.get(&(*rule, p.url.as_str()));
            if i >= contract::MAX_OBSERVATIONS || slots >= contract::MAX_EVALUATIONS {
                excluded += usize::from(found.is_some());
            } else if found.is_some() || instrumented.contains(rule) {
                slots += 1;
                if let Some(k) = found {
                    k_del.push((*k).to_owned());
                }
            }
        }
    }
    assert_eq!(evals.len(), slots);
    let fails = k_ev.len();
    k_ev.sort();
    k_del.sort();
    assert_eq!(k_ev, k_del, "K_ev must equal K_del as multisets");
    if excluded > 0 {
        assert_eq!(doc["evidence_contract"]["truncated"], true);
    }
    (excluded, fails)
}

#[test]
fn evidence_describes_final_delivered_findings() {
    let mut r = policy_report();
    let legacy = audit::analyze(&r.pages, &r.request.url);
    assert!(
        legacy.iter().any(|f| f.rule == "short_title"),
        "fixture must exercise a replaced rule"
    );
    // The truncated-extraction page really delivers a missing_title finding.
    let trunc_key = r
        .findings
        .iter()
        .find(|f| f.rule == "missing_title" && f.url.ends_with("/trunc"))
        .expect("missing_title delivered on the extraction_truncated page")
        .key
        .clone();
    contract::attach_v1_schema4(&mut r).unwrap();

    let (excluded, _) = assert_id_equality(&r);
    assert_eq!(excluded, 0);
    let ec = r.evidence_contract.as_ref().unwrap();
    let ev = ec
        .evaluations
        .iter()
        .find(|e| e.finding_key.as_deref() == Some(trunc_key.as_str()))
        .expect("evaluation slot for the truncated-extraction finding");
    assert_eq!(ev.outcome, Outcome::Fail);

    // The replaced rule points at the policy finding, in the S14 key namespace.
    let keys: HashSet<_> = ec
        .evaluations
        .iter()
        .filter_map(|e| e.finding_key.clone())
        .collect();
    let policy_key = digest(b"seosmith-s14-finding-v1\nshort_title\nhttps://example.com/short");
    assert!(keys.contains(&policy_key));
    let legacy_key = legacy
        .iter()
        .find(|f| f.rule == "short_title")
        .unwrap()
        .key
        .clone();
    assert_ne!(legacy_key, policy_key);
    assert!(!keys.contains(&legacy_key));
}

#[test]
fn caps_exclude_findings_on_both_sides_and_set_truncated() {
    // 200 empty-title HTML pages: 128 observations, evaluations capped at 512.
    let pages: Vec<Page> = (0..200)
        .map(|i| page(&format!("https://example.com/p{i}"), ""))
        .collect();
    let mut r = policy_report_with(pages);
    contract::attach_v1_schema4(&mut r).unwrap();
    let ec = r.evidence_contract.as_ref().unwrap();
    assert_eq!(ec.observations.len(), contract::MAX_OBSERVATIONS);
    assert_eq!(ec.omitted_observations, 200 - contract::MAX_OBSERVATIONS);
    assert_eq!(ec.evaluations.len(), contract::MAX_EVALUATIONS);
    assert!(ec.truncated);
    assert!(!ec.evaluation_coverage_complete);
    let (excluded, fails) = assert_id_equality(&r);
    assert!(excluded > 0);
    assert_eq!(fails, contract::MAX_EVALUATIONS);
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
