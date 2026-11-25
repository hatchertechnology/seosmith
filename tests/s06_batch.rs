use seosmith::s06_batch::{
    BatchError, MAX_COMPARISONS, MAX_INPUT_BYTES, MAX_PAGES, RetainedPage, analyze,
};
use seosmith::s06_detector::Source;

fn page<'a>(id: &'a str, bytes: &'a [u8], status: Option<u16>, complete: bool) -> RetainedPage<'a> {
    RetainedPage {
        identity: id,
        source: Source {
            bytes: Some(bytes),
            status,
            content_type: "text/html; charset=utf-8",
            complete,
        },
    }
}
const A: &[u8] =
    b"<html><body><main>one two three four five six seven eight nine ten</main></body></html>";
const B: &[u8] = b"<html><body><main>one two three four five six seven eight nine ten eleven</main></body></html>";
const C: &[u8] = b"<html><body><main>one two three four five six seven eight nine ten eleven twelve thirteen</main></body></html>";

#[test]
fn stable_edges_and_transitive_groups() {
    let x = analyze(&[
        page("c", C, Some(200), true),
        page("a", A, Some(200), true),
        page("b", B, Some(200), true),
    ])
    .unwrap();
    let y = analyze(&[
        page("b", B, Some(200), true),
        page("c", C, Some(200), true),
        page("a", A, Some(200), true),
    ])
    .unwrap();
    assert_eq!(
        serde_json::to_string(&x).unwrap(),
        serde_json::to_string(&y).unwrap()
    );
    assert_eq!(x.edges.len(), 2);
    assert_eq!(x.groups[0].members, ["a", "b", "c"]);
    assert_eq!(x.groups[0].supporting_edges, [0, 1]);
    assert_eq!(x.no_candidate, 1);
}

#[test]
fn identical_bytes_still_require_complete_eligible_evidence() {
    let good = analyze(&[page("a", A, Some(200), true), page("b", A, Some(200), true)]).unwrap();
    assert_eq!(good.edges.len(), 1);
    for (status, complete) in [(Some(404), true), (None, true), (Some(200), false)] {
        let out = analyze(&[
            page("a", A, Some(200), true),
            page("b", A, status, complete),
        ])
        .unwrap();
        assert!(out.edges.is_empty());
        assert_eq!(out.unknown_reasons.values().sum::<usize>(), 1);
    }
    for bytes in [
        b"<main>Loading...</main>".as_slice(),
        b"<main>\xff</main>".as_slice(),
    ] {
        let out = analyze(&[
            page("a", bytes, Some(200), true),
            page("b", bytes, Some(200), true),
        ])
        .unwrap();
        assert!(out.edges.is_empty());
        assert_eq!(out.unknown_reasons.values().sum::<usize>(), 1);
    }
}

#[test]
fn variants_short_pages_and_error_pages_remain_distinct() {
    let sku1 = b"<main>red widget sku alpha useful product details</main>";
    let sku2 = b"<main>red widget sku beta useful product details</main>";
    let out = analyze(&[
        page("a", sku1, Some(200), true),
        page("b", sku2, Some(200), true),
    ])
    .unwrap();
    assert_eq!(out.no_candidate, 1);
    let out = analyze(&[
        page("a", b"<main>hours today</main>", Some(200), true),
        page("b", b"<main>hours tomorrow</main>", Some(200), true),
    ])
    .unwrap();
    assert_eq!(out.unknown_reasons.values().sum::<usize>(), 1);
    let error = b"<title>Page not found</title><h1>Page not found</h1><main>Sorry this page was not found</main>";
    let p = page("a", error, Some(200), true);
    let batch = analyze(&[
        page("a", error, Some(200), true),
        page("b", error, Some(200), false),
    ])
    .unwrap();
    assert_eq!(batch.pages[0].soft_404_decision, "candidate");
    assert_eq!(batch.pages[1].soft_404_decision, "unknown");
    assert_eq!(
        seosmith::s06_detector::soft_404(&p.source).decision,
        seosmith::s06_detector::Decision::Candidate
    );
}

#[test]
fn limits_and_omissions_are_explicit() {
    let ids: Vec<_> = (0..=MAX_PAGES).map(|i| format!("{i:03}")).collect();
    let pages: Vec<_> = ids.iter().map(|id| page(id, A, Some(200), true)).collect();
    assert!(matches!(analyze(&pages), Err(BatchError::TooManyPages)));
    let out = analyze(&pages[..MAX_PAGES]).unwrap();
    assert_eq!(out.comparisons, MAX_COMPARISONS);
    assert_eq!(
        out.omitted_comparisons,
        MAX_PAGES * (MAX_PAGES - 1) / 2 - MAX_COMPARISONS
    );
    assert_eq!(out.edges.len(), MAX_COMPARISONS);
    assert!(matches!(
        analyze(&[page("x", A, Some(200), true), page("x", A, Some(200), true)]),
        Err(BatchError::DuplicateIdentity)
    ));
    assert!(matches!(
        analyze(&[page("", A, Some(200), true)]),
        Err(BatchError::InvalidIdentity)
    ));
    let big = vec![0; MAX_INPUT_BYTES + 1];
    assert!(matches!(
        analyze(&[page("x", &big, Some(200), true)]),
        Err(BatchError::TooManyBytes)
    ));
}
