use seosmith::{
    model::{Page, Relationship},
    resource_evidence::{
        affected_pages, duplicate_resources, references, resource, resource_byte_groups,
    },
};
use std::collections::BTreeMap;

#[test]
fn headers_signatures_and_decoded_bytes_are_distinct() {
    let headers = BTreeMap::from([
        ("content-length".into(), "12".into()),
        ("content-type".into(), "image/jpeg".into()),
        ("content-encoding".into(), "gzip".into()),
        ("cache-control".into(), "max-age=60".into()),
        ("etag".into(), "\"v1\"".into()),
        ("last-modified".into(), "yesterday".into()),
    ]);
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x02\0\0\0\x03";
    let r = resource("https://example.test/a", &headers, png);
    assert_eq!(
        (
            r.decoded_body_bytes,
            r.declared_content_length,
            r.transfer_bytes
        ),
        (24, Some(12), None)
    );
    assert_eq!(r.content_type.as_deref(), Some("image/jpeg"));
    assert_eq!(
        (r.detected_image_format, r.image_dimensions),
        (Some("png"), Some((2, 3)))
    );
    assert_eq!(r.content_encoding.as_deref(), Some("gzip"));
    assert_eq!(r.cache_control.as_deref(), Some("max-age=60"));
    assert_eq!(r.etag.as_deref(), Some("\"v1\""));
    assert_eq!(r.last_modified.as_deref(), Some("yesterday"));
    let malformed = BTreeMap::from([("content-length".into(), "invalid".into())]);
    let unknown = resource("https://example.test/b", &malformed, b"not an image");
    assert_eq!(
        (
            unknown.declared_content_length,
            unknown.detected_image_format,
            unknown.image_dimensions
        ),
        (None, None, None)
    );
}

#[test]
fn dependency_cycles_and_missing_targets_remain_observed_edges() {
    let rel = |url: &str| Relationship {
        url: url.into(),
        resource: true,
        ..Default::default()
    };
    let html = Page {
        url: "https://example.test/".into(),
        content_type: "text/html".into(),
        status: Some(200),
        relationships: vec![rel("https://example.test/a.css")],
        ..Default::default()
    };
    let css = Page {
        url: "https://example.test/a.css".into(),
        is_resource: true,
        relationships: vec![
            rel("https://example.test/a.css"),
            rel("https://example.test/missing.png"),
        ],
        ..Default::default()
    };
    let affected = affected_pages(&[html, css]);
    assert!(affected["https://example.test/missing.png"].contains("https://example.test/"));
    assert_eq!(affected.len(), 2);
}

#[test]
fn duplicate_and_totals_describe_only_supplied_decoded_bodies() {
    let headers = BTreeMap::from([("content-type".into(), "image/png; charset=binary".into())]);
    let assets = vec![
        resource("https://example.test/a", &headers, b"same"),
        resource("https://example.test/b", &headers, b"same"),
        resource("https://example.test/c", &headers, b"other"),
    ];
    let groups = duplicate_resources(&assets);
    assert_eq!(
        groups[0].urls,
        ["https://example.test/a", "https://example.test/b"]
    );
    let totals = resource_byte_groups(&assets, false);
    assert_eq!(totals[0].decoded_body_bytes, 13);
    assert_eq!(totals[0].content_type.as_deref(), Some("image/png"));
    assert!(!totals[0].complete);
}

#[test]
fn retained_html_references_and_bounds_do_not_infer_rendering() {
    let refs = references(
        "https://example.test/page",
        include_str!("../fixtures/s08/references.html"),
    );
    assert!(refs.complete);
    assert_eq!(refs.source_url, "https://example.test/page");
    assert_eq!(refs.occurrences[0].raw_src, refs.occurrences[1].raw_src);
    assert_eq!(refs.occurrences[0].raw_rel.as_deref(), Some("canonical"));
    assert_eq!(
        refs.occurrences[1].raw_rel.as_deref(),
        Some("stylesheet alternate")
    );
    assert_eq!(
        refs.occurrences[2].raw_srcset.as_deref(),
        Some("small.webp 1x, large.webp 2x")
    );
    assert_eq!(
        refs.occurrences[2].media.as_deref(),
        Some("(min-width: 600px)")
    );
    assert_eq!(
        refs.occurrences[2].declared_type.as_deref(),
        Some("image/webp")
    );
    assert_eq!(refs.occurrences[3].loading.as_deref(), Some("lazy"));
    assert_eq!(refs.occurrences[3].width.as_deref(), Some("320"));
    let bounded = references("source", &format!("<img src='{}'>", "x".repeat(2049)));
    assert!(!bounded.complete);
    assert_eq!(bounded.occurrences[0].raw_src.as_ref().unwrap().len(), 2048);
    let bounded_rel = references(
        "source",
        &format!("<link href='/x' rel='{}'>", "r".repeat(2049)),
    );
    assert!(!bounded_rel.complete);
    assert_eq!(
        bounded_rel.occurrences[0].raw_rel.as_ref().unwrap().len(),
        2048
    );
    let bounded = references("source", &"<img>".repeat(2001));
    assert!(!bounded.complete);
    assert_eq!(bounded.occurrences.len(), 2000);
}
