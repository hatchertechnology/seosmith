use seosmith::{
    audit,
    contract::{self, Outcome},
    model::{CrawlRequest, Page, Report},
};
use serde_json::Value;

fn report(partial: bool) -> Report {
    let request: CrawlRequest = serde_json::from_value(serde_json::json!({
        "request_id":"00000000-0000-4000-8000-000000000001", "website_id":"00000000-0000-4000-8000-000000000002",
        "binding_generation":1, "url":"https://example.com/"
    })).unwrap();
    let pages = vec![
        Page {
            url: "https://example.com/".into(),
            status: Some(200),
            content_type: "text/html".into(),
            encoding: "utf-8".into(),
            title: "An observed nonempty title".into(),
            ..Page::default()
        },
        Page {
            url: "https://example.com/empty".into(),
            status: Some(200),
            content_type: "text/html".into(),
            encoding: "utf-8".into(),
            ..Page::default()
        },
        Page {
            url: "https://example.com/blocked".into(),
            blocked_by_robots: true,
            ..Page::default()
        },
        Page {
            url: "https://example.com/image.png".into(),
            status: Some(200),
            content_type: "image/png".into(),
            is_resource: true,
            ..Page::default()
        },
    ];
    let findings = audit::analyze(&pages, &request.url);
    Report {
        schema_version: 1,
        engine_version: "0.2.0".into(),
        ruleset_version: seosmith::model::RULESET_VERSION.into(),
        request,
        started_at: "2026-11-16T12:00:00Z".into(),
        finished_at: "2026-11-16T12:01:00Z".into(),
        outcome: if partial { "partial" } else { "completed" }.into(),
        stop_reason: if partial {
            "page_limit"
        } else {
            "frontier_exhausted"
        }
        .into(),
        coverage_complete: !partial,
        pages,
        findings,
        evidence_contract: None,
        policy_guidance: None,
    }
}

#[test]
fn legacy_and_versioned_shared_fixtures_match_producer() {
    for (name, partial, v2) in [
        ("legacy", false, false),
        ("v2-complete", false, true),
        ("v2-partial", true, true),
    ] {
        let mut r = report(partial);
        if v2 {
            contract::enable_v2(&mut r).unwrap();
        }
        let path = format!(
            "{}/fixtures/contracts/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        if std::env::var_os("SEOSMITH_UPDATE_FIXTURES").is_some() {
            std::fs::write(&path, serde_json::to_string(&r).unwrap()).unwrap();
        }
        let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), fixture);
        let decoded: Report = serde_json::from_value(fixture).unwrap();
        assert_eq!(decoded.schema_version, r.schema_version);
    }
    let rules =
        serde_json::to_value(audit::rules().iter().map(|r| r.id).collect::<Vec<_>>()).unwrap();
    let path = format!(
        "{}/fixtures/contracts/rules.json",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var_os("SEOSMITH_UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, serde_json::to_string(&rules).unwrap()).unwrap();
    }
    assert_eq!(
        rules,
        serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap()
    );
}

#[test]
fn gate_preserves_legacy_keys_and_never_infers_passes() {
    let mut r = report(false);
    let original = serde_json::to_value(&r).unwrap();
    assert!(original.get("evidence_contract").is_none());
    contract::enable_v2(&mut r).unwrap();
    assert_eq!(
        serde_json::to_value(&r.findings).unwrap(),
        original["findings"]
    );
    let c = r.evidence_contract.as_ref().unwrap();
    assert!(!c.evaluation_coverage_complete);
    let title: Vec<_> = c
        .evaluations
        .iter()
        .filter(|e| e.rule == "missing_title")
        .map(|e| &e.outcome)
        .collect();
    assert_eq!(
        title,
        vec![
            &Outcome::Pass,
            &Outcome::Fail,
            &Outcome::Unknown,
            &Outcome::NotApplicable
        ]
    );
    let legacy = c
        .coverage
        .iter()
        .find(|c| c.rule == "invalid_language")
        .unwrap();
    assert_eq!(legacy.unknown, r.pages.len());
    assert_eq!(legacy.passed, 0);
    for c in &c.coverage {
        assert_eq!(
            c.entities,
            c.passed + c.failed + c.not_applicable + c.unknown
        );
        assert_eq!(
            c.entities,
            c.eligible + c.ineligible + c.eligibility_unknown
        );
        assert_eq!(
            c.entities,
            c.attempted + c.not_attempted + c.attempt_unknown
        );
    }
}

#[test]
fn identity_and_policy_are_stable_but_distinguish_semantics() {
    assert_ne!(
        contract::entity_id("https://example.com/", false),
        contract::entity_id("https://example.com/", true)
    );
    let mut a = report(false);
    let mut b = a.clone();
    b.request.request_id = uuid::Uuid::new_v4();
    b.finished_at = "2026-11-17T12:00:00Z".into();
    contract::enable_v2(&mut a).unwrap();
    contract::enable_v2(&mut b).unwrap();
    assert_eq!(
        a.evidence_contract.unwrap().policy.signature,
        b.evidence_contract.unwrap().policy.signature
    );
    let mut a = report(false);
    let mut b = a.clone();
    b.request.max_resources = 0;
    contract::enable_v2(&mut a).unwrap();
    contract::enable_v2(&mut b).unwrap();
    assert_ne!(
        a.evidence_contract.unwrap().comparison_key,
        b.evidence_contract.unwrap().comparison_key
    );
}

#[test]
fn bounded_metadata_keeps_unknown_coverage_for_omissions() {
    let mut r = report(false);
    r.pages = (0..40_000)
        .map(|i| Page {
            url: format!("https://example.com/{i}"),
            ..Page::default()
        })
        .collect();
    r.findings.clear();
    // Test extension bounds independently of the full report cap with a smaller realistic report.
    r.pages.truncate(1000);
    contract::enable_v2(&mut r).unwrap();
    if std::env::var_os("SEOSMITH_UPDATE_FIXTURES").is_some() {
        std::fs::write(
            format!(
                "{}/fixtures/contracts/v2-truncated.json",
                env!("CARGO_MANIFEST_DIR")
            ),
            serde_json::to_string(&r).unwrap(),
        )
        .unwrap();
    }
    let fixture: Report = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/fixtures/contracts/v2-truncated.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&fixture).unwrap(),
        serde_json::to_value(&r).unwrap()
    );
    let c = r.evidence_contract.unwrap();
    assert_eq!(c.observations.len(), contract::MAX_OBSERVATIONS);
    assert!(c.evaluations.len() <= contract::MAX_EVALUATIONS);
    assert!(contract::serialized_size(&c).unwrap() <= contract::MAX_EXTENSION_BYTES);
    assert!(c.truncated);
    assert!(!c.evaluation_coverage_complete);
    assert_eq!(c.omitted_observations, 1000 - contract::MAX_OBSERVATIONS);
    assert!(
        c.coverage
            .iter()
            .all(|c| c.unknown_reasons["contract_limit"] >= 872)
    );
}

#[test]
fn duplicate_entities_and_oversized_reports_fail_without_mutation() {
    let mut r = report(false);
    r.pages.push(r.pages[0].clone());
    assert!(contract::enable_v2(&mut r).is_err());
    assert_eq!(r.schema_version, 1);
    let mut r = report(false);
    r.pages[0].title = "x".repeat(contract::MAX_REPORT_BYTES);
    assert!(contract::enable_v2(&mut r).is_err());
    assert!(r.evidence_contract.is_none());
}

#[tokio::test]
async fn persisted_v2_retry_retains_exact_bytes_and_digest() {
    let dir = tempfile::tempdir().unwrap();
    let store = seosmith::store::Store::open(dir.path().join("state.db").to_str().unwrap())
        .await
        .unwrap();
    let mut r = report(true);
    store.submit(&r.request).await.unwrap();
    store.claim().await.unwrap();
    contract::enable_v2(&mut r).unwrap();
    store.finish(&r).await.unwrap();
    let (id, bytes, digest, _) = store.pending().await.unwrap().unwrap();
    assert_eq!(digest, seosmith::model::digest(bytes.as_bytes()));
    assert_eq!(store.report(&id).await.unwrap().unwrap(), bytes);
    store.retry(&id, 1).await.unwrap();
    assert_eq!(store.report(&id).await.unwrap().unwrap(), bytes);
}

#[test]
fn nesting_limit_counts_containers_not_string_characters() {
    let mut r = report(false);
    r.pages[0]
        .structured_data
        .push(seosmith::model::StructuredData {
            format: "json_ld".into(),
            data: Some(serde_json::json!({"text": "[[[\"{\\"})),
            error: None,
        });
    assert!(contract::enable_v2(&mut r).is_ok());
    let mut r = report(false);
    let mut value = serde_json::json!(null);
    for _ in 0..64 {
        value = serde_json::json!([value]);
    }
    r.pages[0]
        .structured_data
        .push(seosmith::model::StructuredData {
            format: "json_ld".into(),
            data: Some(value),
            error: None,
        });
    assert!(contract::enable_v2(&mut r).is_err());
    assert_eq!(r.schema_version, 1);
}
