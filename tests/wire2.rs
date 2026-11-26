//! WP8b-3 (design `wave1-wp8a.md` §7 "Wire-2"): the s14-wire-2 serializer.
//!
//! Layout: limits, manifest form, assembly and budget, persistability, flags.
use anyhow::Result;
use seosmith::{
    audit,
    contract::{self, EvidenceLimits},
    model::{CrawlRequest, Page, Report},
    policy::{self, AdvisoryPolicy},
    policy_candidate::ManifestForm,
};

#[path = "../examples/make_scale_fixture.rs"]
mod scale;

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

/// A policy report (awaiting schema 4) with `n` pages that all have an empty title.
fn policy_report(n: usize, seed: &str, form: ManifestForm) -> Report {
    let request: CrawlRequest = serde_json::from_value(serde_json::json!({
        "request_id":"00000000-0000-4000-8000-000000000001",
        "website_id":"00000000-0000-4000-8000-000000000002",
        "binding_generation":1, "url": seed
    }))
    .unwrap();
    let pages: Vec<Page> = (0..n)
        .map(|i| page(&format!("https://example.com/p{i}"), ""))
        .collect();
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
    policy::apply_with_evidence_form(&mut report, &AdvisoryPolicy::default(), form).unwrap();
    report
}

// ---------------------------------------------------------------- limits

#[test]
fn evidence_limits_are_wire_specific_and_wire1_is_the_historical_pair() {
    assert_eq!(EvidenceLimits::WIRE1.observations, 128);
    assert_eq!(EvidenceLimits::WIRE1.evaluations, 512);
    assert_eq!(
        EvidenceLimits::WIRE1.observations,
        contract::MAX_OBSERVATIONS
    );
    assert_eq!(EvidenceLimits::WIRE1.evaluations, contract::MAX_EVALUATIONS);
    assert_eq!(EvidenceLimits::WIRE2.observations, 1024);
    assert_eq!(EvidenceLimits::WIRE2.evaluations, 1024);
}

#[test]
fn wire1_limits_attach_exactly_like_the_v1_entry_point() -> Result<()> {
    let mut a = policy_report(300, "https://example.com/", ManifestForm::List);
    let mut b = a.clone();
    contract::attach_v1_schema4(&mut a)?;
    contract::attach_schema4(&mut b, EvidenceLimits::WIRE1)?;
    assert_eq!(serde_json::to_vec(&a)?, serde_json::to_vec(&b)?);
    assert_eq!(
        a.evidence_contract.as_ref().unwrap().observations.len(),
        128
    );
    Ok(())
}

#[test]
fn wire2_limits_keep_150_pages_untruncated() -> Result<()> {
    let mut r = policy_report(150, "https://example.com/", ManifestForm::Digest);
    contract::attach_schema4(&mut r, EvidenceLimits::WIRE2)?;
    let ec = r.evidence_contract.as_ref().unwrap();
    println!("150 pages: {} evaluations", ec.evaluations.len());
    assert_eq!(ec.version, 1);
    assert_eq!(ec.observations.len(), 150);
    assert_eq!(ec.omitted_observations, 0);
    assert!(!ec.truncated);
    assert!(ec.evaluations.len() > 512 && ec.evaluations.len() <= 1024);
    Ok(())
}

#[test]
fn wire2_evidence_truncates_at_exactly_1024() -> Result<()> {
    let mut r = policy_report(1100, "https://example.com/", ManifestForm::Digest);
    contract::attach_schema4(&mut r, EvidenceLimits::WIRE2)?;
    let ec = r.evidence_contract.as_ref().unwrap();
    assert_eq!(ec.observations.len(), 1024);
    assert_eq!(ec.omitted_observations, 76);
    assert_eq!(ec.evaluations.len(), 1024);
    assert!(ec.truncated);
    Ok(())
}

#[test]
fn worst_case_wire2_evidence_fits_one_mib_with_a_16384_byte_seed() -> Result<()> {
    // 1,100 pages (so both caps bind), then a 16,384 B seed: the evidence
    // extension must stay <= 1 MiB by construction (design A3: ~944 KB).
    // The seed is set after the policy step: S14 itself rejects a seed that long.
    let mut r = policy_report(1100, "https://example.com/", ManifestForm::Digest);
    // Evidence holds hashed entity ids and locators, never URLs, so page URL
    // length does not move it; the seed is the only free long string.
    let seed = format!("https://example.com/{}", "s".repeat(16_384 - 20));
    assert_eq!(seed.len(), 16_384);
    r.request.url = seed.parse()?;
    contract::attach_schema4(&mut r, EvidenceLimits::WIRE2)?;
    let bytes = serde_json::to_vec(&r.evidence_contract)?.len();
    println!("worst-case wire-2 evidence = {bytes} B");
    assert_eq!(
        r.evidence_contract.as_ref().unwrap().observations.len(),
        1_024
    );
    assert!(bytes <= contract::MAX_EXTENSION_BYTES, "{bytes}");
    Ok(())
}

// -------------------------------------------------------------- manifest

use seosmith::{
    generation_error::JobErrorCode,
    policy_candidate::{ManifestRow, candidates_digest, generate_form, generate_unvalidated_form},
};
use sha2::{Digest, Sha256};

fn hex_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn candidates_digest_is_the_a9_framing() {
    assert_eq!(
        candidates_digest(&[]),
        hex_digest(b"seosmith-candidates-v1\ncount=0\nend\n")
    );
    let ids = vec!["sha256:aa".to_owned(), "x|edge:3".to_owned()];
    assert_eq!(
        candidates_digest(&ids),
        hex_digest(b"seosmith-candidates-v1\ncount=2\nid=9:sha256:aa\nid=8:x|edge:3\nend\n")
    );
    // Order is part of the digest; so is length framing (no ambiguity at joins).
    let swapped = vec![ids[1].clone(), ids[0].clone()];
    assert_ne!(candidates_digest(&ids), candidates_digest(&swapped));
    assert_ne!(
        candidates_digest(&["ab".to_owned(), "c".to_owned()]),
        candidates_digest(&["a".to_owned(), "bc".to_owned()])
    );
}

#[test]
fn manifest_rows_serialize_in_exactly_one_form() -> Result<()> {
    let report = scale::scale_report(scale::Shape::site(10, 2))?;
    let (_, list) = generate_form(&report, &AdvisoryPolicy::default(), ManifestForm::List)?;
    let (_, digest) = generate_form(&report, &AdvisoryPolicy::default(), ManifestForm::Digest)?;
    assert_eq!(list.len(), 13);
    assert_eq!(digest.len(), 13);
    for (l, d) in list.iter().zip(&digest) {
        let lv = serde_json::to_value(l)?;
        let dv = serde_json::to_value(d)?;
        assert!(lv.get("candidates").is_some());
        assert!(lv.get("candidates_count").is_none() && lv.get("candidates_digest").is_none());
        assert!(dv.get("candidates").is_none());
        assert_eq!(dv["candidates_count"], l.candidates.len());
        assert_eq!(dv["candidates_digest"], candidates_digest(&l.candidates));
        assert_eq!(dv["category"], lv["category"]);
        assert_eq!(dv["expected_pointer"], lv["expected_pointer"]);
        // Each form round-trips.
        let back: ManifestRow = serde_json::from_value(dv.clone())?;
        assert_eq!(serde_json::to_value(&back)?, dv);
        let back: ManifestRow = serde_json::from_value(lv.clone())?;
        assert_eq!(serde_json::to_value(&back)?, lv);
    }
    Ok(())
}

#[test]
fn a_manifest_row_must_be_exactly_one_form_with_no_unknown_keys() {
    let both = serde_json::json!({"category":"a","rule_id":"b","rule_version":"c","expected_pointer":null,
        "candidates":[], "candidates_count":0, "candidates_digest":"sha256:00"});
    assert!(serde_json::from_value::<ManifestRow>(both).is_err());
    let half = serde_json::json!({"category":"a","rule_id":"b","rule_version":"c","expected_pointer":null,
        "candidates_count":0});
    assert!(serde_json::from_value::<ManifestRow>(half).is_err());
    let unknown = serde_json::json!({"category":"a","rule_id":"b","rule_version":"c","expected_pointer":null,
        "candidates":[], "extra":1});
    assert!(serde_json::from_value::<ManifestRow>(unknown).is_err());
}

#[test]
fn digest_manifest_at_1753_entities_is_bounded_and_matches_the_in_memory_lists() -> Result<()> {
    // The #64 geometry: 57 pages + 1,696 resources, 8 out-of-scope links per page.
    let report = scale::scale_report(scale::Shape::site(1_696, 8))?;
    let policy = AdvisoryPolicy::default();
    let (candidate, list) = generate_unvalidated_form(&report, &policy, ManifestForm::List)?;
    let (_, digest) = generate_unvalidated_form(&report, &policy, ManifestForm::Digest)?;
    let list_bytes = serde_json::to_vec(&list)?.len();
    let digest_bytes = serde_json::to_vec(&digest)?.len();
    println!("manifest at 1753 entities: list = {list_bytes} B, digest = {digest_bytes} B");
    assert!(list_bytes > 262_144);
    assert!(digest_bytes <= 16_384, "{digest_bytes}");
    for (d, l) in digest.iter().zip(&list) {
        let s = d.summary.as_ref().expect("digest form");
        assert_eq!(s.digest, candidates_digest(&l.candidates));
        assert_eq!(s.count, l.candidates.len());
        // The in-memory id list is kept, never serialized.
        assert_eq!(d.candidates, l.candidates);
        let cov = candidate
            .coverage
            .iter()
            .find(|c| c.category == d.category && c.rule_id == d.rule_id)
            .unwrap();
        assert_eq!(s.count, cov.candidates);
    }
    // `manifest_digest` never included candidates: identical across forms.
    assert_eq!(
        seosmith::policy_candidate::manifest_digest(&list),
        seosmith::policy_candidate::manifest_digest(&digest)
    );
    Ok(())
}

#[test]
fn each_form_enforces_its_own_manifest_cap() -> Result<()> {
    let report = scale::scale_report(scale::Shape::site(1_696, 8))?;
    let policy = AdvisoryPolicy::default();
    let error = generate_form(&report, &policy, ManifestForm::List).unwrap_err();
    assert_eq!(error.code, JobErrorCode::PolicyManifestLimit);
    // The digest form delivers the same crawl; its cap is 16,384 B.
    generate_form(&report, &policy, ManifestForm::Digest)?;
    assert_eq!(ManifestForm::List.max_bytes(), 262_144);
    assert_eq!(ManifestForm::Digest.max_bytes(), 16_384);
    Ok(())
}

// -------------------------------------------------------------- assembly

use seosmith::{
    families::types::{
        Coverage, FAMILY_LIMITS, FamilyItem, FamilyKey, FamilySection, FamilySet, Outcome,
        SectionState,
    },
    finding_coverage_prototype,
    wire2::{FAMILIES_MAX_BYTES, generate_wire_v2, generate_wire_v2_with_cap},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const HTP_CAP: usize = 4_952_756;

/// Six `unavailable` sections: what a job with no usable capture carries.
fn unavailable(reason: &str) -> FamilySet {
    FamilySet(
        FamilyKey::ALL
            .iter()
            .map(|k| (*k, FamilySection::unavailable(*k, "test-1", reason)))
            .collect(),
    )
}

fn item_at(report: &Report, page: usize, kind: &str, outcome: Outcome) -> FamilyItem {
    let p = &report.pages[page];
    FamilyItem {
        entity_id: Some(contract::entity_id(&p.url, p.is_resource)),
        locator: Some(format!("/pages/{page}")),
        kind: kind.into(),
        outcome,
        reason: (outcome == Outcome::Unknown).then(|| "target_not_fetched".to_owned()),
        facts: json!({"n": page}).as_object().unwrap().clone(),
    }
}

/// A `complete`-or-`partial` section with `items` and matching bookkeeping.
fn section(key: FamilyKey, items: Vec<FamilyItem>) -> FamilySection {
    let unknown = items
        .iter()
        .filter(|i| i.outcome == Outcome::Unknown)
        .count() as u64;
    let mut s = FamilySection::unavailable(key, "test-1", "x");
    s.state = if unknown == 0 {
        SectionState::Complete
    } else {
        SectionState::Partial
    };
    s.reason = None;
    s.coverage = Coverage {
        entities: items.len() as u64,
        eligible: items.len() as u64,
        attempted: items.len() as u64,
        observed: items.len() as u64 - unknown,
        unknown,
        unknown_reasons: if unknown > 0 {
            BTreeMap::from([("target_not_fetched".to_owned(), unknown)])
        } else {
            BTreeMap::new()
        },
    };
    s.items_total = items.len() as u64;
    s.items = items;
    s
}

fn base_report(resources: usize, external: usize) -> Report {
    scale::scale_report(scale::Shape::site(resources, external)).unwrap()
}

fn generate_v2(report: &mut Report, families: FamilySet) -> Result<Value> {
    let bytes = generate_wire_v2(report, &AdvisoryPolicy::default(), families)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[test]
fn v2_report_carries_the_label_in_all_three_places_and_the_new_member() -> Result<()> {
    let mut report = base_report(20, 2);
    let v = generate_v2(&mut report, unavailable("capture_mode_changed"))?;
    assert_eq!(v["schema_version"], 4);
    assert_eq!(v["policy_guidance"]["version"], "s14-wire-2");
    let canonical: Value = serde_json::from_str(
        v["policy_guidance"]["candidate"]["policy_canonical_json"]
            .as_str()
            .unwrap(),
    )?;
    assert_eq!(canonical["validator_version"], "s14-wire-2");
    assert_eq!(canonical["wire_contract_version"], "s14-wire-2");
    // The record policy_ref binds to the relabelled digest (self-consistency).
    let digest = v["policy_guidance"]["candidate"]["policy_digest"].clone();
    assert!(
        v["policy_guidance"]["candidate"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["policy_ref"] == digest)
    );
    assert_eq!(v["evidence_contract"]["version"], 1);
    assert!(v["finding_coverage"].is_object());
    let ef = &v["evidence_families"];
    assert_eq!(ef["version"], 1);
    assert_eq!(ef["semantics"], "s14-families-1");
    assert_eq!(ef["max_bytes"], FAMILIES_MAX_BYTES);
    assert_eq!(ef["budget_state"], "within_budget");
    let families = ef["families"].as_object().unwrap();
    assert_eq!(families.len(), 6);
    for key in FamilyKey::ALL {
        let s = &families[key.as_str()];
        assert_eq!(s["state"], "unavailable");
        assert_eq!(s["reason"], "capture_mode_changed");
        assert_eq!(s["items"], json!([]));
    }
    Ok(())
}

#[test]
fn v2_manifest_rows_are_digest_form_and_match_coverage() -> Result<()> {
    let mut report = base_report(20, 2);
    let v = generate_v2(&mut report, unavailable("capture_mode_changed"))?;
    let manifest = v["policy_guidance"]["manifest"].as_array().unwrap();
    assert_eq!(manifest.len(), 13);
    let coverage = v["policy_guidance"]["candidate"]["coverage"]
        .as_array()
        .unwrap();
    for row in manifest {
        assert!(row.get("candidates").is_none());
        let c = coverage
            .iter()
            .find(|c| c["category"] == row["category"] && c["rule_id"] == row["rule_id"])
            .unwrap();
        assert_eq!(row["candidates_count"], c["candidates"]);
        assert!(
            row["candidates_digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }
    Ok(())
}

#[test]
fn wire1_output_is_unchanged_by_the_v2_machinery() -> Result<()> {
    let mut report = base_report(20, 2);
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    let v: Value = serde_json::from_slice(&bytes)?;
    assert!(v.get("evidence_families").is_none());
    assert_eq!(v["policy_guidance"]["version"], "s14-wire-1");
    assert!(
        v["policy_guidance"]["manifest"][0]
            .get("candidates")
            .is_some()
    );
    assert_eq!(
        v["evidence_contract"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        77
    );
    // Over the v1 caps the v1 evidence still truncates at 128 observations.
    let mut big = base_report(200, 2);
    let bytes = finding_coverage_prototype::generate_wire_v1(&mut big, &AdvisoryPolicy::default())?;
    let v: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        v["evidence_contract"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        128
    );
    Ok(())
}

#[test]
fn v2_s01_evidence_is_untruncated_up_to_1024() -> Result<()> {
    let mut report = base_report(200, 2); // 257 entities
    let v = generate_v2(&mut report, unavailable("capture_mode_changed"))?;
    assert_eq!(
        v["evidence_contract"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        257
    );
    assert_eq!(v["evidence_contract"]["truncated"], false);
    Ok(())
}

#[test]
fn families_are_sorted_bound_and_counted() -> Result<()> {
    let mut report = base_report(20, 0);
    let mut set = unavailable("capture_mode_changed");
    // Out of order on purpose; one `unknown` carries its reason.
    let items = vec![
        item_at(&report, 5, "orphan_page", Outcome::Pass),
        item_at(&report, 9, "orphan_page", Outcome::Issue),
        item_at(&report, 2, "orphan_page", Outcome::Unknown),
        item_at(&report, 1, "orphan_page", Outcome::Issue),
        item_at(&report, 3, "deep_page", Outcome::Observed),
    ];
    set.0.insert(FamilyKey::S05, section(FamilyKey::S05, items));
    let v = generate_v2(&mut report, set)?;
    let s = &v["evidence_families"]["families"]["s05_link_graph"];
    let order: Vec<(String, String)> = s["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["outcome"].as_str().unwrap().to_owned(),
                i["locator"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("issue", "/pages/1"),
            ("issue", "/pages/9"),
            ("unknown", "/pages/2"),
            ("observed", "/pages/3"),
            ("pass", "/pages/5"),
        ]
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    assert_eq!(s["items_total"], 5);
    assert_eq!(s["omitted_items"], 0);
    assert_eq!(s["truncated"], false);
    assert_eq!(s["omitted_reason"], Value::Null);
    assert_eq!(s["state"], "partial");
    assert_eq!(s["max_items"], 256);
    assert_eq!(s["max_bytes"], 131_072);
    Ok(())
}

#[test]
fn the_item_cap_omits_with_family_item_limit() -> Result<()> {
    let mut report = base_report(300, 0);
    let items: Vec<_> = (0..300)
        .map(|i| item_at(&report, i, "orphan_page", Outcome::Issue))
        .collect();
    let mut set = unavailable("capture_mode_changed");
    set.0.insert(FamilyKey::S05, section(FamilyKey::S05, items));
    let v = generate_v2(&mut report, set)?;
    let s = &v["evidence_families"]["families"]["s05_link_graph"];
    assert_eq!(s["items"].as_array().unwrap().len(), 256);
    assert_eq!(s["omitted_items"], 44);
    assert_eq!(s["items_total"], 300);
    assert_eq!(s["truncated"], true);
    assert_eq!(s["omitted_reason"], "family_item_limit");
    assert_eq!(
        s["state"], "partial",
        "a truncated section is never complete"
    );
    // S06 allows only 128.
    let mut report = base_report(300, 0);
    let items: Vec<_> = (0..200)
        .map(|i| item_at(&report, i, "exact_duplicate", Outcome::Issue))
        .collect();
    let mut set = unavailable("capture_mode_changed");
    set.0.insert(FamilyKey::S06, section(FamilyKey::S06, items));
    let v = generate_v2(&mut report, set)?;
    let s = &v["evidence_families"]["families"]["s06_duplicates"];
    assert_eq!(s["items"].as_array().unwrap().len(), 128);
    assert_eq!(s["max_items"], 128);
    assert_eq!(s["omitted_items"], 72);
    Ok(())
}

#[test]
fn a_section_that_breaks_an_invariant_becomes_unavailable_not_a_failure() -> Result<()> {
    let base = base_report(20, 0);
    let good = |base: &Report| vec![item_at(base, 1, "orphan_page", Outcome::Issue)];
    let mut cases: Vec<(&str, FamilySection)> = Vec::new();
    // unresolved locator
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].locator = Some("/pages/9999".into());
    cases.push(("unresolved locator", s));
    // non-canonical index
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].locator = Some("/pages/01".into());
    cases.push(("non-canonical index", s));
    // link index out of range
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].locator = Some("/pages/1/links/9999".into());
    cases.push(("link out of range", s));
    // entity does not match the locator
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].entity_id = Some(contract::entity_id("https://elsewhere.example/", false));
    cases.push(("entity mismatch", s));
    // an unbound item needs facts.url
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].locator = None;
    s.items[0].entity_id = None;
    cases.push(("unbound without url", s));
    // unknown without a reason
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].outcome = Outcome::Unknown;
    cases.push(("unknown without reason", s));
    // a capture omission read as pass
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].outcome = Outcome::Pass;
    s.items[0].reason = Some("collector_byte_limit".into());
    cases.push(("omission as pass", s));
    // facts too big
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0]
        .facts
        .insert("blob".into(), Value::String("z".repeat(2_000)));
    s.items[0]
        .facts
        .insert("blob2".into(), Value::String("z".repeat(2_000)));
    s.items[0]
        .facts
        .insert("blob3".into(), Value::String("z".repeat(2_000)));
    cases.push(("facts over 4096", s));
    // string over 2048
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0]
        .facts
        .insert("blob".into(), Value::String("z".repeat(2_049)));
    cases.push(("string over 2048", s));
    // facts too deep
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0]
        .facts
        .insert("d".into(), json!({"a": {"b": {"c": {"d": 1}}}}));
    cases.push(("facts depth", s));
    // NUL
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].facts.insert("x".into(), "a\0b".into());
    cases.push(("NUL", s));
    // bad code characters
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].kind = "Orphan Page".into();
    cases.push(("kind characters", s));
    // unknown_reasons do not sum
    let mut s = section(FamilyKey::S05, good(&base));
    s.coverage.unknown = 3;
    cases.push(("unknown_reasons sum", s));
    // items_total wrong
    let mut s = section(FamilyKey::S05, good(&base));
    s.items_total = 7;
    cases.push(("items_total", s));
    // truncated without omissions
    let mut s = section(FamilyKey::S05, good(&base));
    s.truncated = true;
    cases.push(("truncated flag", s));
    // omission with an unknown reason
    let mut s = section(FamilyKey::S05, good(&base));
    s.omitted_items = 1;
    s.items_total = 2;
    s.truncated = true;
    s.omitted_reason = Some("because".into());
    cases.push(("omitted_reason vocabulary", s));
    // complete with unknowns
    let mut s = section(FamilyKey::S05, good(&base));
    s.coverage.unknown = 1;
    s.coverage.unknown_reasons = BTreeMap::from([("x".to_owned(), 1)]);
    cases.push(("complete with unknown", s));
    // unavailable with items
    let mut s = section(FamilyKey::S05, good(&base));
    s.state = SectionState::Unavailable;
    s.reason = Some("x".into());
    cases.push(("unavailable with items", s));
    // summary over 8 KiB
    let mut s = section(FamilyKey::S05, good(&base));
    s.summary = json!({"blob": "z".repeat(9_000)});
    cases.push(("summary size", s));
    // 8b-3 review: each of these was accepted by the producer and rejected by the receiver.
    // summary not an object
    let mut s = section(FamilyKey::S05, good(&base));
    s.summary = json!([1, 2]);
    cases.push(("summary not an object", s));
    // more unknown items than coverage.unknown
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].outcome = Outcome::Unknown;
    s.items[0].reason = Some("target_not_fetched".into());
    s.state = SectionState::Partial;
    cases.push(("unknown items over coverage.unknown", s));
    // an unbound item with an empty facts.url
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].locator = None;
    s.items[0].entity_id = None;
    s.items[0].facts.insert("url".into(), "".into());
    cases.push(("unbound with empty url", s));
    // an object key over 2,048 B
    let mut s = section(FamilyKey::S05, good(&base));
    s.items[0].facts.insert("k".repeat(2_049), 1.into());
    cases.push(("facts key over 2048", s));
    // summary string over 2,048 B
    let mut s = section(FamilyKey::S05, good(&base));
    s.summary = json!({"blob": "z".repeat(2_049)});
    cases.push(("summary string over 2048", s));
    // summary key over 2,048 B
    let mut s = section(FamilyKey::S05, good(&base));
    s.summary = json!({"k".repeat(2_049): 1});
    cases.push(("summary key over 2048", s));
    // coverage.unknown_reasons overflow
    let mut s = section(FamilyKey::S05, good(&base));
    s.coverage.unknown_reasons = BTreeMap::from([("a".to_owned(), u64::MAX), ("b".to_owned(), 1)]);
    cases.push(("unknown_reasons overflow", s));
    // items_total overflow
    let mut s = section(FamilyKey::S05, good(&base));
    s.omitted_items = u64::MAX;
    s.truncated = true;
    s.omitted_reason = Some("family_item_limit".into());
    s.items_total = 0;
    cases.push(("items_total overflow", s));
    for (name, broken) in cases {
        let mut report = base_report(20, 0);
        let mut set = unavailable("capture_mode_changed");
        set.0.insert(FamilyKey::S05, broken);
        let v = generate_v2(&mut report, set).unwrap_or_else(|e| panic!("{name}: {e}"));
        let s = &v["evidence_families"]["families"]["s05_link_graph"];
        assert_eq!(s["state"], "unavailable", "{name}");
        assert_eq!(s["reason"], "analyzer_error", "{name}");
        assert_eq!(s["items"], json!([]), "{name}");
    }
    // The same inputs, repaired, are accepted.
    let mut report = base_report(20, 0);
    let mut set = unavailable("capture_mode_changed");
    set.0
        .insert(FamilyKey::S05, section(FamilyKey::S05, good(&report)));
    let v = generate_v2(&mut report, set)?;
    assert_eq!(
        v["evidence_families"]["families"]["s05_link_graph"]["state"],
        "complete"
    );
    Ok(())
}

/// An unavailable family with a long builder reason does not push the object
/// over a tight budget: the skeleton uses the shortest sections, and the shrink
/// loop rewrites an `unavailable` section too.
#[test]
fn unavailable_sections_with_longer_reasons_fit_the_tightest_budget() -> Result<()> {
    let (base, skeleton, _) = skeleton_sizes(40)?;
    let key = ",\"evidence_families\":".len();
    let long = "x".repeat(64);
    // Exactly the room the skeleton needs under `report_byte_budget`.
    let cap = base + key + 65_536 + skeleton + STATE_SLACK;
    let mut report = base_report(40, 0);
    let mut set = unavailable("capture_mode_changed");
    let mut with_summary = FamilySection::unavailable(FamilyKey::S02, "test-1", &long);
    with_summary.summary = json!({"note": "n".repeat(4_000)});
    set.0.insert(FamilyKey::S02, with_summary);
    let bytes = generate_wire_v2_with_cap(&mut report, &AdvisoryPolicy::default(), set, cap)?;
    assert!(bytes.len() <= cap);
    let v: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(v["evidence_families"]["budget_state"], "report_byte_budget");
    for (k, _) in FAMILY_LIMITS {
        assert_eq!(
            v["evidence_families"]["families"][k.as_str()]["state"],
            "unavailable"
        );
    }
    Ok(())
}

/// Whatever the builder emits, the output obeys every rule the receiver checks
/// beyond the ones above: sorted, within each family's caps, summary an object.
#[test]
fn assembled_sections_obey_the_receivers_ordering_and_caps() -> Result<()> {
    let mut report = base_report(40, 0);
    let set = heavy_set(&report, 1_500);
    let v = generate_v2(&mut report, set)?;
    for (k, l) in FAMILY_LIMITS {
        let s = &v["evidence_families"]["families"][k.as_str()];
        assert!(s["summary"].is_object());
        assert_eq!(s["max_items"], l.max_items);
        assert_eq!(s["max_bytes"], l.max_bytes);
        assert!(s["items"].as_array().unwrap().len() <= l.max_items);
        assert!(serde_json::to_vec(s)?.len() <= l.max_bytes);
        let rank = |o: &str| {
            ["issue", "unknown", "observed", "pass"]
                .iter()
                .position(|x| *x == o)
        };
        let keys: Vec<_> = s["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    rank(i["outcome"].as_str().unwrap()),
                    i["locator"].as_str().map(str::to_owned),
                    i["kind"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{}", k.as_str());
    }
    Ok(())
}

#[test]
fn a_missing_family_is_filled_as_unavailable() -> Result<()> {
    let mut report = base_report(20, 0);
    let mut set = unavailable("capture_mode_changed");
    set.0.remove(&FamilyKey::S04);
    let v = generate_v2(&mut report, set)?;
    let s = &v["evidence_families"]["families"]["s04_structured_data"];
    assert_eq!(s["state"], "unavailable");
    assert_eq!(s["reason"], "analyzer_error");
    Ok(())
}

#[test]
fn unbound_and_relationship_and_link_locators_resolve() -> Result<()> {
    let mut report = base_report(20, 2);
    let mut fragment = item_at(&report, 1, "broken_fragment", Outcome::Issue);
    fragment.locator = Some("/pages/1/links/0".into());
    let mut resource = item_at(&report, 1, "resource_link", Outcome::Issue);
    resource.locator = Some("/pages/1/relationships/0".into());
    let unbound = FamilyItem {
        entity_id: None,
        locator: None,
        kind: "sitemap_not_crawled".into(),
        outcome: Outcome::Issue,
        reason: None,
        facts: json!({"url": "https://scale.example/not-crawled"})
            .as_object()
            .unwrap()
            .clone(),
    };
    let mut set = unavailable("capture_mode_changed");
    set.0.insert(
        FamilyKey::S03,
        section(FamilyKey::S03, vec![fragment, resource, unbound]),
    );
    let v = generate_v2(&mut report, set)?;
    let s = &v["evidence_families"]["families"]["s03_integrity"];
    assert_eq!(s["state"], "complete");
    assert_eq!(s["items"].as_array().unwrap().len(), 3);
    Ok(())
}

// ---------------------------------------------------------------- budget

/// Items of about `bytes` serialized bytes each, as a wire-2 section.
fn heavy_section(report: &Report, key: FamilyKey, count: usize, pad: usize) -> FamilySection {
    let items = (0..count)
        .map(|i| {
            let mut item = item_at(
                report,
                i % report.pages.len(),
                "orphan_page",
                Outcome::Issue,
            );
            item.facts
                .insert("pad".into(), Value::String("p".repeat(pad)));
            item.facts.insert("i".into(), json!(i));
            item
        })
        .collect();
    section(key, items)
}

fn heavy_set(report: &Report, pad: usize) -> FamilySet {
    let mut set = unavailable("capture_mode_changed");
    for (key, limits) in FAMILY_LIMITS {
        set.0
            .insert(key, heavy_section(report, key, limits.max_items, pad));
    }
    set
}

fn families_len(v: &Value) -> usize {
    serde_json::to_vec(&v["evidence_families"]).unwrap().len()
}

#[test]
fn a_full_set_is_cut_to_each_familys_byte_cap_and_the_total_cap() -> Result<()> {
    let mut report = base_report(40, 0);
    let set = heavy_set(&report, 1_500);
    let bytes = generate_wire_v2(&mut report, &AdvisoryPolicy::default(), set)?;
    let v: Value = serde_json::from_slice(&bytes)?;
    let total = families_len(&v);
    println!(
        "families object = {total} B of {FAMILIES_MAX_BYTES}; report = {} B",
        bytes.len()
    );
    assert!(total <= FAMILIES_MAX_BYTES);
    assert_eq!(v["evidence_families"]["budget_state"], "within_budget");
    for (key, limits) in FAMILY_LIMITS {
        let s = &v["evidence_families"]["families"][key.as_str()];
        let len = serde_json::to_vec(s)?.len();
        assert!(len <= limits.max_bytes, "{} {len}", key.as_str());
        assert_eq!(s["truncated"], true, "{}", key.as_str());
        assert_eq!(
            s["omitted_reason"],
            "family_byte_budget",
            "{}",
            key.as_str()
        );
        assert_eq!(s["state"], "partial");
        let kept = s["items"].as_array().unwrap().len() as u64;
        assert_eq!(
            s["items_total"].as_u64().unwrap(),
            kept + s["omitted_items"].as_u64().unwrap()
        );
        assert_eq!(s["items_total"].as_u64().unwrap(), limits.max_items as u64);
        // Greedy fit: one more item would not have fit.
        assert!(
            len + 1_700 > limits.max_bytes,
            "{} left {} B unused",
            key.as_str(),
            limits.max_bytes - len
        );
    }
    Ok(())
}

/// The skeleton is budgeted with the longer `report_byte_budget` state string.
const STATE_SLACK: usize = "report_byte_budget".len() - "within_budget".len();

/// Bytes the report takes with all-`unavailable` families, and the object size.
fn skeleton_sizes(resources: usize) -> Result<(usize, usize, usize)> {
    let mut report = base_report(resources, 0);
    let bytes = generate_wire_v2(
        &mut report,
        &AdvisoryPolicy::default(),
        unavailable("report_byte_budget"),
    )?;
    let v: Value = serde_json::from_slice(&bytes)?;
    let object = families_len(&v);
    let base = bytes.len() - object - ",\"evidence_families\":".len();
    Ok((base, object, bytes.len()))
}

#[test]
fn the_budget_degrades_to_report_byte_budget_when_the_report_is_nearly_full() -> Result<()> {
    let (base, skeleton, _) = skeleton_sizes(40)?;
    let key = ",\"evidence_families\":".len();
    // Room for 300,000 B of families.
    let cap = base + key + 65_536 + 300_000;
    let mut report = base_report(40, 0);
    let set = heavy_set(&report, 1_500);
    let bytes = generate_wire_v2_with_cap(&mut report, &AdvisoryPolicy::default(), set, cap)?;
    let v: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(v["evidence_families"]["budget_state"], "report_byte_budget");
    assert!(families_len(&v) <= 300_000, "{}", families_len(&v));
    assert!(bytes.len() + 65_536 <= cap + key, "reserve is kept");
    assert!(bytes.len() <= cap);
    for (k, _) in FAMILY_LIMITS {
        let s = &v["evidence_families"]["families"][k.as_str()];
        assert_eq!(s["truncated"], true);
        assert_eq!(s["omitted_reason"], "report_byte_budget", "{}", k.as_str());
    }
    // At the smallest budget the skeleton fits and every family is `unavailable`
    // or empty-but-valid: no failure.
    let cap = base + key + 65_536 + skeleton + STATE_SLACK;
    let mut report = base_report(40, 0);
    let set = heavy_set(&report, 1_500);
    let bytes = generate_wire_v2_with_cap(&mut report, &AdvisoryPolicy::default(), set, cap)?;
    let v: Value = serde_json::from_slice(&bytes)?;
    assert!(bytes.len() <= cap);
    assert!(families_len(&v) <= skeleton + STATE_SLACK);
    assert_eq!(v["evidence_families"]["budget_state"], "report_byte_budget");
    Ok(())
}

#[test]
fn the_base_report_fails_with_report_byte_limit_exactly_over_the_cap() -> Result<()> {
    let (base, skeleton, _) = skeleton_sizes(40)?;
    let key = ",\"evidence_families\":".len();
    // The skeleton with the real semantics and the longest reason: use the tight cap.
    let fits = base + key + 65_536 + skeleton + STATE_SLACK;
    let mut report = base_report(40, 0);
    generate_wire_v2_with_cap(
        &mut report,
        &AdvisoryPolicy::default(),
        unavailable("report_byte_budget"),
        fits,
    )?;
    let mut report = base_report(40, 0);
    let error = generate_wire_v2_with_cap(
        &mut report,
        &AdvisoryPolicy::default(),
        unavailable("report_byte_budget"),
        fits - 1,
    )
    .unwrap_err();
    assert_eq!(error.code, JobErrorCode::ReportByteLimit);
    // And a base that is itself over the cap fails the same way.
    let mut report = base_report(40, 0);
    let error = generate_wire_v2_with_cap(
        &mut report,
        &AdvisoryPolicy::default(),
        unavailable("x"),
        base - 1,
    )
    .unwrap_err();
    assert_eq!(error.code, JobErrorCode::ReportByteLimit);
    Ok(())
}

#[test]
fn one_byte_either_side_of_the_full_budget() -> Result<()> {
    // `within_budget` exactly when the room (cap - reserve - key - base) >= 786,432.
    let (base, _, _) = skeleton_sizes(40)?;
    let key = ",\"evidence_families\":".len();
    let edge = base + key + 65_536 + FAMILIES_MAX_BYTES;
    for (cap, expected) in [
        (edge - 1, "report_byte_budget"),
        (edge, "within_budget"),
        (edge + 1, "within_budget"),
    ] {
        let mut report = base_report(40, 0);
        let set = heavy_set(&report, 1_500);
        let bytes = generate_wire_v2_with_cap(&mut report, &AdvisoryPolicy::default(), set, cap)?;
        let v: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(
            v["evidence_families"]["budget_state"], expected,
            "cap {cap}"
        );
        assert!(bytes.len() <= cap);
    }
    Ok(())
}

#[test]
fn the_production_cap_is_the_htp_cap_and_the_output_is_persistable() -> Result<()> {
    let mut report = base_report(40, 0);
    let set = heavy_set(&report, 1_500);
    let bytes = generate_wire_v2(&mut report, &AdvisoryPolicy::default(), set)?;
    assert!(bytes.len() <= HTP_CAP);
    let v: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        v["request"]["request_id"],
        report.request.request_id.to_string()
    );
    Ok(())
}

// ----------------------------------------------------------------- scale

#[test]
fn the_64_geometry_delivers_under_v2_with_bounded_extensions() -> Result<()> {
    // 57 pages + 1,696 resources, 8 out-of-scope links per page, with a
    // near-maximum seed URL: the worst-case canonical policy. S14 bounds every
    // string at 16,384 B and the canonical policy repeats the seed, so the
    // longest seed that delivers is a few hundred bytes shorter than 16,384.
    let mut report = base_report(1_696, 8);
    let seed = format!("{}/{}", scale::SCALE_HOST, "s".repeat(15_500));
    report.request.url = seed.parse()?;
    let started = std::time::Instant::now();
    let set = unavailable("capture_mode_changed");
    let bytes = generate_wire_v2(&mut report, &AdvisoryPolicy::default(), set)?;
    println!(
        "v2 at 1753 entities: {} B in {:?}",
        bytes.len(),
        started.elapsed()
    );
    let v: Value = serde_json::from_slice(&bytes)?;
    let manifest = serde_json::to_vec(&v["policy_guidance"]["manifest"])?.len();
    let guidance = serde_json::to_vec(&v["policy_guidance"])?.len();
    println!("manifest = {manifest} B, policy_guidance = {guidance} B");
    assert!(manifest <= 16_384, "{manifest}");
    assert!(guidance <= 556_000, "{guidance}");
    assert!(bytes.len() <= HTP_CAP);
    // S01 truncates at exactly 1,024 observations and 1,024 evaluations.
    let ec = &v["evidence_contract"];
    assert_eq!(ec["observations"].as_array().unwrap().len(), 1_024);
    assert!(ec["evaluations"].as_array().unwrap().len() <= 1_024);
    assert_eq!(ec["truncated"], true);
    // Digests equal candidates_digest of the in-memory lists.
    let policy = AdvisoryPolicy::default();
    let mut again = base_report(1_696, 8);
    again.request.url = report.request.url.clone();
    let (_, list) = generate_unvalidated_form(&again, &policy, ManifestForm::List)?;
    for row in v["policy_guidance"]["manifest"].as_array().unwrap() {
        let l = list
            .iter()
            .find(|l| l.category == row["category"] && l.rule_id == row["rule_id"])
            .unwrap();
        assert_eq!(row["candidates_count"], l.candidates.len());
        assert_eq!(row["candidates_digest"], candidates_digest(&l.candidates));
    }
    Ok(())
}

#[test]
fn policy_guidance_stays_under_556_kb_with_10000_external_links() -> Result<()> {
    // 57 + 1,696 entities and ~10,000 out-of-scope links (175 on each of 57
    // pages) with a near-maximum seed: the design's `policy_guidance` bound.
    let mut report = base_report(1_696, 175);
    let seed = format!("{}/{}", scale::SCALE_HOST, "s".repeat(15_500));
    report.request.url = seed.parse()?;
    let bytes = generate_wire_v2(&mut report, &AdvisoryPolicy::default(), unavailable("x"))?;
    let v: Value = serde_json::from_slice(&bytes)?;
    let manifest = serde_json::to_vec(&v["policy_guidance"]["manifest"])?.len();
    let guidance = serde_json::to_vec(&v["policy_guidance"])?.len();
    let edges = v["policy_guidance"]["manifest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rule_id"] == "external_link")
        .unwrap()["candidates_count"]
        .as_u64()
        .unwrap();
    println!("edges = {edges}, manifest = {manifest} B, policy_guidance = {guidance} B");
    assert!(edges >= 9_900, "{edges}");
    assert!(manifest <= 16_384, "{manifest}");
    assert!(guidance <= 556_000, "{guidance}");
    Ok(())
}
