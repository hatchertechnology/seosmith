#[path = "../src/s06_detector.rs"]
mod detector;
use detector::{Decision, Source};
use serde_json::Value;
use std::fs;

#[test]
fn labeled_sources_drive_detector_without_label_inputs() {
    let labels: Value = serde_json::from_str(include_str!("../fixtures/s06/labels.json")).unwrap();
    assert_eq!(detector::SEMANTICS, "s06-local-detector-1");
    for case in labels["cases"].as_array().unwrap() {
        let read = |name: &str| fs::read(format!("fixtures/s06/{name}")).unwrap();
        let bytes = case["html"].as_str().map(read);
        let peer_bytes = case["related"]
            .as_array()
            .unwrap()
            .first()
            .and_then(Value::as_str)
            .map(read);
        let response = &case["response"];
        let source = Source {
            bytes: bytes.as_deref(),
            status: response["status"]
                .as_u64()
                .map(|n| u16::try_from(n).unwrap()),
            content_type: response["content_type"].as_str().unwrap(),
            complete: response["availability"] == "available"
                || response["availability"] == "http_error"
                || response["availability"] == "missing_status",
        };
        let peer = peer_bytes.as_deref().map(|b| Source {
            bytes: Some(b),
            status: Some(200),
            content_type: "text/html",
            complete: true,
        });
        let out = if case["family"] == "soft_404" {
            detector::soft_404(&source)
        } else {
            detector::compare(&source, peer.as_ref())
        };
        let expected = match case["expected"].as_bool() {
            Some(true) => Decision::Candidate,
            Some(false) => Decision::NoCandidate,
            None => Decision::Unknown,
        };
        assert_eq!(out.decision, expected, "{}: {}", case["id"], out.reason);
        if let Some(e) = out.evidence {
            assert_eq!(e.source_sha256, case["source_sha256"].as_str().unwrap());
            assert_ne!(e.main_sha256, "");
            assert!(e.excerpt.chars().count() <= 240);
            assert!((0.0..=1.0).contains(&e.template_fraction));
        }
        if peer.is_some() {
            assert!(out.similarity.is_some());
        }
    }
}

#[test]
fn limits_and_missing_evidence_remain_unknown() {
    let large = vec![b'x'; 256 * 1024 + 1];
    let s = Source {
        bytes: Some(&large),
        status: Some(200),
        content_type: "text/html",
        complete: true,
    };
    assert_eq!(detector::soft_404(&s).decision, Decision::Unknown);
    let page = Source {
        bytes: Some(b"<main>Useful content</main>"),
        ..s
    };
    assert_eq!(detector::compare(&page, None).decision, Decision::Unknown);
    assert_eq!(detector::soft_404(&page).decision, Decision::NoCandidate);
}
