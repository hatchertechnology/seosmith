#[path = "../src/s06_detector.rs"]
mod detector;
use detector::{Decision, Source};
use serde_json::Value;
use std::fs;

#[test]
fn labeled_sources_drive_detector_without_label_inputs() {
    let labels: Value = serde_json::from_str(include_str!("../fixtures/s06/labels.json")).unwrap();
    assert_eq!(detector::SEMANTICS, "s06-local-detector-2");
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

#[test]
fn loading_main_is_unknown_in_either_comparison_position() {
    let a = Source {
        bytes: Some("<title>A</title><main>Loading…</main>".as_bytes()),
        status: Some(200),
        content_type: "text/html",
        complete: true,
    };
    let b = Source {
        bytes: Some(b"<title>B</title><main>Loading...</main>"),
        ..a
    };
    let useful = Source {
        bytes: Some(b"<main>Office hours Monday Friday</main>"),
        ..a
    };
    for (left, right) in [(&a, &b), (&a, &useful), (&useful, &a)] {
        let out = detector::compare(left, Some(right));
        assert_eq!(out.decision, Decision::Unknown);
        assert!(out.similarity.is_none());
        assert!(out.peer_evidence.is_some());
    }
    assert_eq!(detector::soft_404(&b).decision, Decision::Unknown);
}

#[test]
fn pair_evidence_identifies_peer_and_failed_side() {
    let a = Source {
        bytes: Some(b"<main>a b c d</main>"),
        status: Some(200),
        content_type: "text/html",
        complete: true,
    };
    let b = Source {
        bytes: Some(b"<title>B</title><main>a b c d e</main>"),
        ..a
    };
    let c = Source {
        bytes: Some(b"<title>C</title><main>a b c d f</main>"),
        ..a
    };
    let ab = detector::compare(&a, Some(&b));
    let ac = detector::compare(&a, Some(&c));
    assert_eq!(ab.similarity, ac.similarity);
    let peer_b = ab.peer_evidence.unwrap();
    let peer_c = ac.peer_evidence.unwrap();
    assert_ne!(peer_b.source_sha256, peer_c.source_sha256);
    assert_ne!(peer_b.main_sha256, peer_c.main_sha256);
    assert_eq!(peer_b.title, "B");
    assert_eq!(peer_b.excerpt, "a b c d e");
    let missing = Source { bytes: None, ..a };
    assert_eq!(
        detector::compare(&a, Some(&missing)).unavailable_side,
        Some(detector::Side::Peer)
    );
    assert_eq!(
        detector::compare(&missing, Some(&a)).unavailable_side,
        Some(detector::Side::Source)
    );
}

#[test]
fn low_information_comparisons_and_exact_byte_gate() {
    let short = Source {
        bytes: Some(b"<main>Welcome</main>"),
        status: Some(200),
        content_type: "text/html",
        complete: true,
    };
    let other = Source {
        bytes: Some(b"<title>Different</title><main>Welcome</main>"),
        ..short
    };
    for (a, b) in [(&short, &other), (&other, &short)] {
        let out = detector::compare(a, Some(b));
        assert_eq!(out.decision, Decision::Unknown);
        assert!(out.similarity.is_none());
    }
    assert_eq!(detector::soft_404(&short).decision, Decision::NoCandidate);
    assert_eq!(
        detector::compare(&short, Some(&short)).reason,
        "identical source bytes"
    );
    let shell = Source {
        bytes: Some(b"<body>Welcome</body>"),
        ..short
    };
    assert_eq!(
        detector::compare(&shell, Some(&shell)).decision,
        Decision::Unknown
    );
}
