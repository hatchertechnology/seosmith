//! WP8b-4 (design `wave1-wp8a.md` section 7, "Fixtures and parity"): the committed
//! `fixtures/wire2/**` set.
//!
//! - every file is exactly what `examples/make_wire2_fixtures.rs` generates now
//!   (regeneration is byte-for-byte; nothing is hand-edited);
//! - the manifest lists every file with its size, sha256 and the expected `admit`
//!   stage and error name, which follow the design's section 5 stage table;
//! - the positives are real wire-2 reports and the negatives each carry the defect
//!   they are named for;
//! - `scripts/verify-s14-schema4.py --wire2 --admit` compares the HTP receiver's
//!   `rejected stage=.. error=..` line with the manifest. When a receiver run has
//!   been recorded in the manifest (`admit` per file), it must equal `expect`.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt::Write as _, path::PathBuf, process::Command};

#[path = "../examples/make_wire2_fixtures.rs"]
mod fixtures_gen;

const CAP: usize = 4_952_756;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dir() -> PathBuf {
    root().join("fixtures/wire2")
}

fn manifest() -> Value {
    serde_json::from_slice(&std::fs::read(dir().join("manifest.json")).expect("manifest.json"))
        .unwrap()
}

fn files() -> Vec<Value> {
    manifest()["files"].as_array().unwrap().clone()
}

fn read(path: &str) -> Vec<u8> {
    std::fs::read(dir().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn doc(path: &str) -> Value {
    serde_json::from_slice(&read(path)).unwrap()
}

fn entry(name: &str) -> Value {
    files()
        .into_iter()
        .find(|f| f["path"] == name)
        .unwrap_or_else(|| panic!("{name} is not in the manifest"))
}

fn strip_admit(mut manifest: Value) -> Value {
    let o = manifest.as_object_mut().unwrap();
    o.retain(|k, _| !k.starts_with("admit"));
    for f in o["files"].as_array_mut().unwrap() {
        f.as_object_mut().unwrap().remove("admit");
    }
    manifest
}

// ------------------------------------------------------------ the set itself

#[test]
fn fixtures_regenerate_byte_for_byte() {
    let cases = fixtures_gen::cases().expect("generate");
    let listed: Vec<String> = files()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_owned())
        .collect();
    let generated: Vec<String> = cases.iter().map(|c| c.path.clone()).collect();
    assert_eq!(
        listed, generated,
        "the manifest lists the generator's files in order"
    );
    for c in &cases {
        let on_disk = read(&c.path);
        assert_eq!(on_disk.len(), c.bytes.len(), "{} length", c.path);
        assert!(
            on_disk == c.bytes,
            "{} differs from its regeneration",
            c.path
        );
    }
    assert_eq!(
        strip_admit(manifest()),
        fixtures_gen::manifest(&cases).unwrap(),
        "manifest.json differs from its regeneration (recorded admit results aside)"
    );
}

#[test]
fn manifest_matches_the_files_the_cap_and_the_directory() {
    let m = manifest();
    assert_eq!(m["max_report_bytes"], CAP);
    assert_eq!(seosmith::delivery::HTP_MAX_REPORT_BYTES, CAP);
    assert_eq!(m["v1_revision"], fixtures_gen::V1_REVISION);
    assert_eq!(m["v2_revision"], fixtures_gen::V2_REVISION);
    let mut listed = BTreeSet::new();
    for f in files() {
        let path = f["path"].as_str().unwrap();
        let bytes = read(path);
        assert_eq!(bytes.len(), f["bytes"], "{path} bytes");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            f["sha256"],
            "{path} sha256"
        );
        assert_eq!(bytes.first(), Some(&b'{'), "{path}");
        assert_eq!(
            bytes.last(),
            Some(&b'}'),
            "{path}: exact wire bytes, no newline"
        );
        // The fixtures are the compact, key-sorted writer's own output.
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            serde_json::to_vec(&value).unwrap(),
            bytes,
            "{path} is canonical"
        );
        if path.starts_with("positive/") && path != "positive/exact-cap-v2.json" {
            assert!(bytes.len() <= CAP, "{path} is over the cap");
        }
        assert!(listed.insert(path.to_owned()), "{path} listed twice");
    }
    let mut on_disk = BTreeSet::new();
    for sub in ["positive", "negative"] {
        for e in std::fs::read_dir(dir().join(sub)).unwrap() {
            on_disk.insert(format!(
                "{sub}/{}",
                e.unwrap().file_name().to_str().unwrap()
            ));
        }
    }
    assert_eq!(listed, on_disk, "manifest and directories agree");
    assert_eq!(read("positive/exact-cap-v2.json").len(), CAP);
    assert_eq!(read("negative/cap-plus-one-v2.json").len(), CAP + 1);
}

#[test]
fn the_set_has_every_design_fixture_and_at_least_eighteen_negatives() {
    let paths: BTreeSet<String> = files()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_owned())
        .collect();
    for name in [
        "complete-small",
        "partial-sources",
        "capture-omissions",
        "family-item-limit",
        "family-byte-budget",
        "report-budget-degraded",
        "exact-cap-v2",
        "wire2-limits",
        "manifest-scale",
        "s14-record-limit",
        "entities-1024",
    ] {
        assert!(
            paths.contains(&format!("positive/{name}.json")),
            "positive {name}"
        );
    }
    for name in [
        "families-null-v1",
        "families-null-v2",
        "families-unknown-coverage-key",
        "families-unknown-item-key",
        "manifest-list-under-v2",
        "manifest-digest-under-v1",
        "manifest-count-mismatch",
        "manifest-digest-mismatch",
        "s01-obs-1025-v2",
        "s01-evals-1025-v2",
        "manifest-over-16384-v2",
    ] {
        assert!(
            paths.contains(&format!("negative/{name}.json")),
            "negative {name}"
        );
    }
    let negatives = paths.iter().filter(|p| p.starts_with("negative/")).count();
    assert!(negatives >= 18, "{negatives} negatives");
}

// ------------------------------------------------- the section 5 stage table

fn expect_of(name: &str) -> (bool, String, String) {
    let e = entry(name)["expect"].clone();
    (
        e["admit"].as_bool().unwrap(),
        e["stage"].as_str().unwrap_or("").to_owned(),
        e["error"]
            .as_str()
            .unwrap_or(e["validator_revision"].as_str().unwrap_or(""))
            .to_owned(),
    )
}

#[test]
fn expected_stages_follow_the_design_table() {
    // (fixture, stage, error): section 5 "Rejections", transcribed independently
    // of the generator.
    let table = [
        ("negative/families-null-v1.json", "inspection", "Shape"),
        ("negative/families-object-v1.json", "inspection", "Shape"),
        ("negative/families-null-v2.json", "inspection", "Shape"),
        ("negative/families-missing-v2.json", "inspection", "Shape"),
        (
            "negative/families-unknown-top-key.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-unknown-section-key.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-unknown-coverage-key.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-unknown-item-key.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-items-total-mismatch.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-truncated-mismatch.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-complete-with-unknown.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-unknown-without-reason.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-pass-with-capture-omission.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-locator-unresolved.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-null-locator-no-url.json",
            "inspection",
            "Shape",
        ),
        (
            "negative/families-facts-too-big.json",
            "inspection",
            "Bounds",
        ),
        (
            "negative/families-items-over-max.json",
            "inspection",
            "Bounds",
        ),
        (
            "negative/manifest-list-under-v2.json",
            "inspection",
            "Bounds",
        ),
        (
            "negative/manifest-over-16384-v2.json",
            "inspection",
            "Bounds",
        ),
        ("negative/s01-obs-1025-v2.json", "inspection", "Bounds"),
        ("negative/s01-evals-1025-v2.json", "inspection", "Bounds"),
        (
            "negative/manifest-count-mismatch.json",
            "summary",
            "SemanticMismatch",
        ),
        (
            "negative/manifest-digest-mismatch.json",
            "summary",
            "SemanticMismatch",
        ),
        (
            "negative/manifest-digest-under-v1.json",
            "summary",
            "SemanticMismatch",
        ),
        (
            "negative/population-duplicate-finding-v2.json",
            "population",
            "SemanticMismatch",
        ),
        ("negative/cap-plus-one-v2.json", "inspection", "ByteLimit"),
    ];
    for (name, stage, error) in table {
        let (admit, s, e) = expect_of(name);
        assert!(!admit, "{name}");
        assert_eq!((s.as_str(), e.as_str()), (stage, error), "{name}");
    }
    for f in files() {
        let e = &f["expect"];
        if e["admit"] == true {
            assert_eq!(
                e["validator_revision"],
                fixtures_gen::V2_REVISION,
                "{}",
                f["path"]
            );
            assert!(f["path"].as_str().unwrap().starts_with("positive/"));
        } else {
            assert!(f["path"].as_str().unwrap().starts_with("negative/"));
            // No fixture expects `commit`: SQL-only rejections are tested in 8c-2.
            assert!(
                ["inspection", "summary", "population"].contains(&e["stage"].as_str().unwrap()),
                "{}: stage {}",
                f["path"],
                e["stage"]
            );
            assert!(
                [
                    "Shape",
                    "Bounds",
                    "UnsupportedSchema",
                    "SemanticMismatch",
                    "ByteLimit"
                ]
                .contains(&e["error"].as_str().unwrap()),
                "{}",
                f["path"]
            );
        }
    }
}

// --------------------------------------------------------------- positives

const KEYS: [&str; 6] = [
    "s02_indexing",
    "s03_integrity",
    "s04_structured_data",
    "s05_link_graph",
    "s06_duplicates",
    "s08_resources",
];

fn is_positive(f: &Value) -> bool {
    f["path"].as_str().unwrap().starts_with("positive/")
}

#[test]
fn every_positive_is_a_wire2_report_with_six_families_and_digest_rows() {
    for f in files().iter().filter(|f| is_positive(f)) {
        let path = f["path"].as_str().unwrap();
        let d = doc(path);
        assert_eq!(d["schema_version"], 4, "{path}");
        assert_eq!(d["policy_guidance"]["version"], "s14-wire-2", "{path}");
        let canon: Value = serde_json::from_str(
            d["policy_guidance"]["candidate"]["policy_canonical_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(canon["validator_version"], "s14-wire-2", "{path}");
        assert_eq!(canon["wire_contract_version"], "s14-wire-2", "{path}");
        let rows = d["policy_guidance"]["manifest"].as_array().unwrap();
        assert_eq!(rows.len(), 13, "{path}");
        for row in rows {
            let keys: BTreeSet<&str> = row
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                BTreeSet::from([
                    "category",
                    "rule_id",
                    "rule_version",
                    "expected_pointer",
                    "candidates_count",
                    "candidates_digest"
                ]),
                "{path}: a digest row and nothing else"
            );
            let digest = row["candidates_digest"].as_str().unwrap();
            assert!(
                digest.len() == 71
                    && digest.starts_with("sha256:")
                    && digest[7..]
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{path}: {digest}"
            );
        }
        assert!(
            serde_json::to_vec(&d["policy_guidance"]["manifest"])
                .unwrap()
                .len()
                <= 16_384,
            "{path}: v2 manifest cap"
        );
        let fam = &d["evidence_families"];
        assert_eq!(fam["version"], 1, "{path}");
        assert_eq!(fam["semantics"], "s14-families-1", "{path}");
        assert_eq!(fam["max_bytes"], 786_432, "{path}");
        assert!(serde_json::to_vec(fam).unwrap().len() <= 786_432, "{path}");
        for key in KEYS {
            let s = &fam["families"][key];
            assert!(s.is_object(), "{path}: {key}");
            // The producer replaces a section that breaks an invariant by
            // `unavailable`/`analyzer_error`; no fixture may carry that by accident.
            assert_ne!(s["reason"], "analyzer_error", "{path}: {key} was degraded");
            let items = s["items"].as_array().unwrap().len() as u64;
            assert_eq!(
                s["items_total"].as_u64().unwrap(),
                items + s["omitted_items"].as_u64().unwrap()
            );
        }
        assert!(
            d["evidence_contract"]["observations"]
                .as_array()
                .unwrap()
                .len()
                <= 1024
        );
        assert!(
            d["evidence_contract"]["evaluations"]
                .as_array()
                .unwrap()
                .len()
                <= 1024
        );
    }
}

#[test]
fn each_positive_shows_what_it_is_named_for() {
    let s = |path: &str, key: &str| doc(path)["evidence_families"]["families"][key].clone();

    // complete-small: S06 is always partial (near-duplicate similarity is unknown).
    for key in [
        "s02_indexing",
        "s03_integrity",
        "s04_structured_data",
        "s05_link_graph",
        "s08_resources",
    ] {
        assert_eq!(
            s("positive/complete-small.json", key)["state"],
            "complete",
            "{key}"
        );
    }
    let s06 = s("positive/complete-small.json", "s06_duplicates");
    assert_eq!(s06["state"], "partial");
    assert_eq!(
        s06["coverage"]["unknown_reasons"]["main_text_not_retained"],
        s06["coverage"]["eligible"]
    );

    // partial-sources: reasons on partial sections, one unavailable.
    let p = "positive/partial-sources.json";
    assert_eq!(s(p, "s02_indexing")["reason"], "analyzer_row_limit");
    assert_eq!(s(p, "s03_integrity")["reason"], "sitemap_evidence_mismatch");
    assert_eq!(s(p, "s04_structured_data")["state"], "unavailable");
    assert_eq!(s(p, "s04_structured_data")["reason"], "analysis_deadline");
    assert_eq!(s(p, "s05_link_graph")["reason"], "link_limit");

    // capture-omissions: unknown with the capture reason, never pass.
    let p = "positive/capture-omissions.json";
    let mut seen = BTreeSet::new();
    for key in KEYS {
        let sec = s(p, key);
        for item in sec["items"].as_array().unwrap() {
            if let Some(r) = item["reason"].as_str()
                && (r == "collector_byte_limit" || r == "run_byte_limit")
            {
                assert_eq!(item["outcome"], "unknown", "{key}");
                seen.insert(r.to_owned());
            }
        }
    }
    assert_eq!(
        seen,
        BTreeSet::from([
            "collector_byte_limit".to_owned(),
            "run_byte_limit".to_owned()
        ])
    );
    assert_eq!(s(p, "s08_resources")["reason"], "run_byte_limit");

    // item and byte truncation.
    let sec = s("positive/family-item-limit.json", "s06_duplicates");
    assert_eq!(sec["omitted_reason"], "family_item_limit");
    assert_eq!(sec["items"].as_array().unwrap().len(), 128);
    assert_eq!(sec["truncated"], true);
    let sec = s("positive/family-byte-budget.json", "s03_integrity");
    assert_eq!(sec["omitted_reason"], "family_byte_budget");
    assert!(serde_json::to_vec(&sec).unwrap().len() <= 163_840);
    assert_eq!(
        doc("positive/family-byte-budget.json")["evidence_families"]["budget_state"],
        "within_budget"
    );

    // report budget degradation.
    let d = doc("positive/report-budget-degraded.json");
    assert_eq!(d["evidence_families"]["budget_state"], "report_byte_budget");
    let reasons: BTreeSet<String> = KEYS
        .iter()
        .filter_map(|k| {
            d["evidence_families"]["families"][k]["omitted_reason"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert!(reasons.contains("report_byte_budget"), "{reasons:?}");

    // limits: more than the wire-1 observation limit, untruncated.
    let d = doc("positive/wire2-limits.json");
    assert!(
        d["evidence_contract"]["observations"]
            .as_array()
            .unwrap()
            .len()
            > 128
    );
    assert!(
        d["evidence_contract"]["evaluations"]
            .as_array()
            .unwrap()
            .len()
            > 512
    );
    assert_eq!(d["evidence_contract"]["truncated"], false);

    // unbound item.
    let sec = s("positive/unbound-url-item.json", "s03_integrity");
    let item = &sec["items"][0];
    assert!(item["locator"].is_null() && item["entity_id"].is_null());
    assert!(item["facts"]["url"].as_str().is_some_and(|u| !u.is_empty()));

    // locators at the last link and relationship.
    let sec = s("positive/locators-v2.json", "s05_link_graph");
    let locs: BTreeSet<&str> = sec["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["locator"].as_str())
        .collect();
    assert!(locs.contains("/pages/0/links/22") && locs.contains("/pages/0/relationships/22"));

    // S14 records capped: per-rule omissions in finding_coverage.
    let d = doc("positive/s14-record-limit.json");
    let cov = d["policy_guidance"]["candidate"]["coverage"]
        .as_array()
        .unwrap();
    let omitted: Vec<&Value> = cov
        .iter()
        .filter(|c| c["omitted"].as_u64().unwrap() > 0)
        .collect();
    assert!(omitted.len() >= 5, "per-rule record omissions expected");
    for c in &omitted {
        assert_eq!(c["omission_reason"], "record_limit");
        assert_eq!(
            c["candidates"].as_u64().unwrap(),
            c["emitted"].as_u64().unwrap() + c["omitted"].as_u64().unwrap()
        );
    }
    assert_eq!(d["finding_coverage"]["audit"], "partial");

    // manifest-scale: the geometry that fails under wire-1.
    let d = doc("positive/manifest-scale.json");
    assert!(d["pages"].as_array().unwrap().len() >= 1_753);
    let edges: usize = d["pages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["is_resource"] != true)
        .map(|p| {
            p["links"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|l| {
                    l["url"]
                        .as_str()
                        .is_some_and(|u| u.contains("external.example"))
                })
                .count()
        })
        .sum();
    assert!(edges >= 400, "{edges} out-of-scope edges");
    assert!(
        serde_json::to_vec(&d["policy_guidance"]["manifest"])
            .unwrap()
            .len()
            <= 16_384
    );
    assert_eq!(
        doc("positive/entities-1024.json")["pages"]
            .as_array()
            .unwrap()
            .len(),
        1_024
    );
}

// --------------------------------------------------------------- negatives

#[test]
fn every_negative_differs_from_every_positive_and_is_valid_json() {
    let positives: BTreeSet<Vec<u8>> = files()
        .iter()
        .filter(|f| is_positive(f))
        .map(|f| read(f["path"].as_str().unwrap()))
        .collect();
    let mut seen = BTreeSet::new();
    for f in files().iter().filter(|f| !is_positive(f)) {
        let path = f["path"].as_str().unwrap();
        let bytes = read(path);
        serde_json::from_slice::<Value>(&bytes).unwrap();
        assert!(!positives.contains(&bytes), "{path} equals a positive");
        assert!(seen.insert(bytes), "{path} duplicates another negative");
    }
}

#[test]
fn negatives_carry_the_defect_they_are_named_for() {
    let fam = |p: &str| doc(p)["evidence_families"].clone();
    assert!(fam("negative/families-null-v1.json").is_null());
    assert_eq!(
        doc("negative/families-null-v1.json")["policy_guidance"]["version"],
        "s14-wire-1"
    );
    assert_eq!(
        doc("negative/families-object-v1.json")["policy_guidance"]["version"],
        "s14-wire-1"
    );
    assert!(fam("negative/families-object-v1.json").is_object());
    assert!(fam("negative/families-null-v2.json").is_null());
    assert!(
        doc("negative/families-missing-v2.json")
            .get("evidence_families")
            .is_none()
    );
    assert!(fam("negative/families-not-object-v2.json").is_array());
    assert_eq!(fam("negative/families-bad-version.json")["version"], 2);
    // Label negatives.
    assert_eq!(
        doc("negative/unknown-wire-label.json")["policy_guidance"]["version"],
        "s14-wire-3"
    );
    assert!(
        doc("negative/unknown-wire-label.json")
            .get("evidence_families")
            .is_none()
    );
    assert!(
        doc("negative/unknown-wire-label-with-families.json")
            .get("evidence_families")
            .is_some()
    );
    // Manifest forms.
    let row0 = |p: &str| doc(p)["policy_guidance"]["manifest"][0].clone();
    assert!(
        row0("negative/manifest-list-under-v2.json")
            .get("candidates")
            .is_some()
    );
    assert!(
        row0("negative/manifest-row-missing-digest.json")
            .get("candidates_digest")
            .is_none()
    );
    assert_eq!(
        row0("negative/manifest-row-bad-digest.json")["candidates_digest"],
        "sha256:zz"
    );
    assert!(
        row0("negative/manifest-row-extra-key.json")
            .get("extra")
            .is_some()
    );
    let v1_rows = doc("negative/manifest-digest-under-v1.json");
    assert_eq!(v1_rows["policy_guidance"]["version"], "s14-wire-1");
    assert!(
        v1_rows["policy_guidance"]["manifest"][0]
            .get("candidates_digest")
            .is_some()
    );
    let over = serde_json::to_vec(
        &doc("negative/manifest-over-16384-v2.json")["policy_guidance"]["manifest"],
    )
    .unwrap();
    assert!(over.len() > 16_384);
    // S01 counts.
    let d = doc("negative/s01-obs-1025-v2.json");
    assert_eq!(
        d["evidence_contract"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        1_025
    );
    let d = doc("negative/s01-evals-1025-v2.json");
    assert_eq!(
        d["evidence_contract"]["evaluations"]
            .as_array()
            .unwrap()
            .len(),
        1_025
    );
    // Count and digest.
    let good = doc("positive/complete-small.json");
    let bad = doc("negative/manifest-count-mismatch.json");
    assert_eq!(
        bad["policy_guidance"]["manifest"][0]["candidates_count"]
            .as_u64()
            .unwrap(),
        good["policy_guidance"]["manifest"][0]["candidates_count"]
            .as_u64()
            .unwrap()
            + 1
    );
}

// --------------------------------------------------------- the verify script

fn python(args: &[&str]) -> std::process::Output {
    Command::new("python3")
        .arg(root().join("scripts/verify-s14-schema4.py"))
        .args(args)
        .output()
        .expect("python3")
}

/// A stand-in for the receiver's `admit` example: it answers from a table of
/// `file name -> stdout line`, keyed by the report's sha256, so the script's
/// parsing of the stage line is tested without the HTP repository.
fn fake_admit(table: &[(String, String)]) -> PathBuf {
    let dir = tempfile::tempdir().unwrap().keep();
    let path = dir.join("admit");
    let mut script =
        String::from("#!/bin/sh\nsum=$(sha256sum \"$1\" | cut -d' ' -f1)\ncase \"$sum\" in\n");
    for (sha, body) in table {
        let _ = writeln!(script, "{sha}) {body} ;;");
    }
    script.push_str("*) echo unexpected >&2; exit 2 ;;\nesac\n");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// sha256 of the one byte `S` a fake receiver writes to stdout.
fn s_sha() -> String {
    format!("{:x}", Sha256::digest(b"S"))
}

fn answers(shift: bool) -> Vec<(String, String)> {
    files()
        .iter()
        .map(|f| {
            let e = &f["expect"];
            let body = if e["admit"] == true {
                format!(
                    "printf 'S' ; echo 'validator_revision={} sha256=sha256:{} bytes=1 source_sha256=x source_bytes=1' >&2",
                    e["validator_revision"].as_str().unwrap(),
                    s_sha(),
                )
            } else {
                let stage = if shift { "population" } else { e["stage"].as_str().unwrap() };
                format!(
                    "echo 'noise from the receiver' >&2; echo 'rejected stage={stage} error={}' >&2; exit 1",
                    e["error"].as_str().unwrap()
                )
            };
            (f["sha256"].as_str().unwrap().to_owned(), body)
        })
        .collect()
}

#[test]
fn the_wire2_manifest_verifies_without_a_receiver() {
    let out = python(&["--wire2"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("wire2 fixtures verified"));
}

#[test]
fn the_script_compares_the_receivers_stage_line_with_the_manifest() {
    let ok = fake_admit(&answers(false));
    let out = python(&["--wire2", "--admit", ok.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The same answers with every rejection moved to `population` must fail
    // (positives are unchanged), naming the first mismatch.
    let wrong = fake_admit(&answers(true));
    let out = python(&["--wire2", "--admit", wrong.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("stage") && err.contains("population"), "{err}");
}

#[test]
fn the_script_rejects_an_admitted_negative_and_a_rejected_positive() {
    let mut table = answers(false);
    // Make the first negative admitted.
    let neg = files()
        .iter()
        .position(|f| f["expect"]["admit"] == false)
        .unwrap();
    table[neg].1 = format!(
        "printf 'S'; echo 'validator_revision={} sha256=sha256:{} bytes=1 source_sha256=x source_bytes=1' >&2",
        fixtures_gen::V2_REVISION,
        s_sha()
    );
    let out = python(&["--wire2", "--admit", fake_admit(&table).to_str().unwrap()]);
    assert!(!out.status.success());
    let mut table = answers(false);
    let pos = files()
        .iter()
        .position(|f| f["expect"]["admit"] == true)
        .unwrap();
    table[pos].1 = "echo 'rejected stage=inspection error=Shape' >&2; exit 1".into();
    let out = python(&["--wire2", "--admit", fake_admit(&table).to_str().unwrap()]);
    assert!(!out.status.success());
}

#[test]
fn the_wire1_mode_of_the_script_still_works() {
    let out = python(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_recorded_receiver_run_equals_the_expectation() {
    // `verify-s14-schema4.py --wire2 --admit <bin> --write-admit --receiver-commit <sha>`
    // records what the receiver said per file. When present, it must equal `expect`.
    let m = manifest();
    let recorded: Vec<Value> = files()
        .into_iter()
        .filter(|f| f.get("admit").is_some())
        .collect();
    if recorded.is_empty() {
        assert!(m.get("admit_receiver_commit").is_none());
        return;
    }
    assert_eq!(
        recorded.len(),
        files().len(),
        "a recorded run covers every file"
    );
    assert!(
        m["admit_receiver_commit"]
            .as_str()
            .is_some_and(|c| c.len() == 40)
    );
    for f in recorded {
        let (a, e) = (&f["admit"], &f["expect"]);
        if e["admit"] == true {
            assert_eq!(
                a["validator_revision"], e["validator_revision"],
                "{}",
                f["path"]
            );
        } else {
            assert_eq!(a["stage"], e["stage"], "{}", f["path"]);
            assert_eq!(a["error"], e["error"], "{}", f["path"]);
        }
    }
    let _ = json!(null);
}

// ------------------------------------------------------- the contract document

/// `98304` as `98,304`, the way the document writes it.
fn commas(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn contract() -> String {
    std::fs::read_to_string(root().join("docs/contracts/s14-wire-2.md")).unwrap()
}

#[test]
fn the_contract_document_states_what_the_design_requires() {
    let doc = contract();
    for needle in [
        // The one sentence about the revision string (review R2-9).
        "`full-population-1` in the revision refers to **population equality for findings and entities**",
        "does **not** mean that the S14 policy records are complete",
        // The schema-3 tools (F17).
        "`scripts/check-s14.mjs`",
        "schema-3, wire-1-candidate tools",
        "fails closed on every schema-4 report",
        "make-s14-review-",
        // The flags, in full (A6).
        "`SEOSMITH_POLICY_WIRE_V2`",
        "`SEOSMITH_POLICY_WIRE_V1=true`",
        "`SEOSMITH_EVIDENCE_CONTRACT_V2`",
        // The A9 framing and the stage line.
        "seosmith-candidates-v1",
        "`rejected stage=<inspection|summary|population> error=<InspectionError name>`",
        "s14-wire-2/full-population-1/evidence-v1/families-v1",
    ] {
        assert!(
            doc.contains(needle),
            "docs/contracts/s14-wire-2.md lacks: {needle}"
        );
    }
}

#[test]
fn the_contract_document_names_every_fixture_and_the_digest_examples_hold() {
    let doc = contract();
    for f in files() {
        let path = f["path"].as_str().unwrap();
        let name = path.rsplit('/').next().unwrap().trim_end_matches(".json");
        assert!(
            doc.contains(&format!("`{name}`")),
            "the contract document does not name {name}"
        );
    }
    let a = format!("sha256:{}", "a".repeat(64));
    let b = format!("sha256:{}", "b".repeat(64));
    for (ids, digest) in [
        (
            vec![],
            "sha256:580fb46eb6e09e55506812715ac9f331223231c5b85678faa68d6a1fb94fe765",
        ),
        (
            vec![a, b],
            "sha256:08dec4d63f03d14837de29538c1b8ba7118f2bfd59915371ffd6cfe8add7a29b",
        ),
    ] {
        assert_eq!(seosmith::policy_candidate::candidates_digest(&ids), digest);
        assert!(doc.contains(digest), "the document must carry {digest}");
    }
}

#[test]
fn the_document_table_of_caps_matches_the_producers_constants() {
    use seosmith::families::types::FAMILY_LIMITS;
    let doc = contract();
    let mut total = 0;
    for (key, limits) in FAMILY_LIMITS {
        total += limits.max_bytes;
        let row = doc
            .lines()
            .find(|l| l.starts_with(&format!("| `{}`", key.as_str())))
            .unwrap_or_else(|| panic!("no table row for {}", key.as_str()));
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        assert_eq!(cells[3], limits.max_items.to_string(), "{row}");
        assert_eq!(cells[4], commas(limits.max_bytes), "{row}");
    }
    assert_eq!(total, 786_432);
    assert_eq!(seosmith::wire2::FAMILIES_MAX_BYTES, 786_432);
}
