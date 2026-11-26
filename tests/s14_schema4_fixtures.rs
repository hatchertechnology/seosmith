//! S2: the committed schema-4 fixtures match their manifest, are inside the
//! HTP cap, and satisfy the E-p ID-equality over their serialised bytes
//! (including the crawl fixture, S1 review MINOR-4).

mod support {
    pub mod id_equality;
}

use sha2::{Digest, Sha256};
use support::id_equality::assert_id_equality;

const DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/contracts/s14-schema4-v1"
);
const CAP: usize = 4_952_756;

fn manifest() -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(format!("{DIR}/manifest.json")).unwrap()).unwrap()
}

fn entries() -> Vec<serde_json::Value> {
    manifest()["files"].as_array().unwrap().clone()
}

#[test]
fn cap_constant_matches_the_producer() {
    assert_eq!(seosmith::delivery::HTP_MAX_REPORT_BYTES, CAP);
    assert_eq!(manifest()["max_report_bytes"], CAP);
}

#[test]
fn fixtures_match_the_manifest_and_the_cap() {
    let entries = entries();
    assert!(entries.len() >= 9);
    for entry in &entries {
        let path = entry["path"].as_str().unwrap();
        let bytes = std::fs::read(format!("{DIR}/{path}")).unwrap();
        assert_eq!(bytes.len(), entry["bytes"], "{path} bytes");
        assert!(bytes.len() <= CAP, "{path} exceeds the cap");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            entry["sha256"].as_str().unwrap(),
            "{path} sha256"
        );
        assert_eq!(bytes.last(), Some(&b'}'), "{path}: exact wire bytes");
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["schema_version"], 4, "{path}");
        assert_eq!(doc["evidence_contract"]["version"], 1, "{path}");
        assert!(doc["finding_coverage"].is_object(), "{path}");
    }
    let exact = std::fs::metadata(format!("{DIR}/exact-cap.json")).unwrap();
    assert_eq!(exact.len(), CAP as u64);
}

#[test]
fn every_fixture_satisfies_id_equality_in_the_serialised_bytes() {
    for entry in entries() {
        let path = entry["path"].as_str().unwrap();
        let bytes = std::fs::read(format!("{DIR}/{path}")).unwrap();
        let result = assert_id_equality(&bytes);
        assert_eq!(result.evaluations, entry["evaluations"], "{path}");
        assert_eq!(result.truncated, entry["evidence_truncated"], "{path}");
        assert!(result.fails > 0, "{path}: no fail evaluation to compare");
    }
}

#[test]
fn caps_fixture_is_truncated_and_the_crawl_fixture_is_within_n_m4() {
    let find = |name: &str| {
        entries()
            .into_iter()
            .find(|e| e["path"] == name)
            .unwrap_or_else(|| panic!("{name} listed"))
    };
    let bytes = std::fs::read(format!("{DIR}/mixed-caps-exceeded.json")).unwrap();
    let capped = assert_id_equality(&bytes);
    assert!(capped.truncated && capped.excluded > 0);

    // N-M4: the crawl fixture is evaluated within the Q2 margin and untruncated.
    let crawl = find("crawl.json");
    let bytes = std::fs::read(format!("{DIR}/crawl.json")).unwrap();
    let result = assert_id_equality(&bytes);
    assert!(!result.truncated);
    assert_eq!(result.excluded, 0);
    assert!(
        result.evaluations <= 460,
        "{} evaluations",
        result.evaluations
    );
    assert_eq!(crawl["evaluations"], result.evaluations);
}
