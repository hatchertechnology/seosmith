use seosmith::graph_evidence::{extract, graph, resource, similarity};
use std::collections::BTreeMap;
use url::Url;
fn page(path: &str, html: &str) -> seosmith::graph_evidence::LocalPageEvidence {
    extract(
        &Url::parse(&format!("https://example.test{path}")).unwrap(),
        html,
        200,
    )
}
#[test]
fn graph_uses_navigation_not_discovery_and_preserves_fragments() {
    let home = page(
        "/",
        "<nav><a href='/a#part'>Go</a><a href='/a#other'>Again</a></nav>",
    );
    let a = page("/a", "<main id=part>Useful page</main>");
    let sitemap_only = page("/sitemap-only", "<main>Another page</main>");
    assert_eq!(home.links[0].fragment.as_deref(), Some("part"));
    assert_eq!(home.links[0].fetch_url, "https://example.test/a");
    assert_eq!(home.links[0].context, "nav");
    assert!(a.ids.contains("part"));
    let nodes = graph("https://example.test/", &[home, a, sitemap_only], true);
    assert_eq!(nodes[1].navigation_distance, Some(1));
    assert_eq!(nodes[1].unique_inlinks, 1);
    assert_eq!(nodes[2].navigation_distance, None);
    assert_eq!(nodes[2].reachability, "not_observed_reachable");
}
#[test]
fn incomplete_crawl_does_not_claim_unreachable() {
    let nodes = graph(
        "https://example.test/",
        &[page("/", ""), page("/other", "")],
        false,
    );
    assert_eq!(nodes[1].reachability, "unknown");
}
#[test]
fn content_signals_are_review_only_and_empty_is_not_similarity() {
    let product = page(
        "/product",
        "<title>Blue mug</title><main><h1>Blue mug</h1>Useful ceramic mug</main>",
    );
    let error = page("/missing", "<title>Page not found</title><h1>404</h1>");
    let shell = page("/shell", "<div id=app></div><script>render()</script>");
    assert!(product.content.soft_404_signals.is_empty());
    assert!(
        error
            .content
            .soft_404_signals
            .contains(&"error_title_or_heading".into())
    );
    assert!(
        shell
            .content
            .soft_404_signals
            .contains(&"empty_static_main_text".into())
    );
    assert!(shell.content.review_only);
    assert_eq!(similarity(&shell.content, &product.content), None);
}
#[test]
fn resource_counts_are_explicit_and_image_signature_beats_header() {
    let mut headers = BTreeMap::new();
    headers.insert("content-length".into(), "12".into());
    headers.insert("content-type".into(), "image/jpeg".into());
    let bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x02\0\0\0\x03";
    let r = resource("https://example.test/image", &headers, bytes);
    assert_eq!(r.decoded_body_bytes, 24);
    assert_eq!(r.declared_content_length, Some(12));
    assert_eq!(r.transfer_bytes, None);
    assert_eq!(r.detected_image_format, Some("png"));
    assert_eq!(r.image_dimensions, Some((2, 3)));
}
#[test]
fn fragment_absence_requires_observed_target() {
    use seosmith::graph_evidence::fragment_checks;
    let home = page(
        "/",
        "<a href='/a#yes'>yes</a><a href='/a#no'>no</a><a href='/missing#x'>x</a><a href='/a#%79es'>encoded</a>",
    );
    let target = page("/a", "<div id=yes></div>");
    let checks = fragment_checks(&[home, target]);
    assert_eq!(
        checks.iter().map(|c| c.state).collect::<Vec<_>>(),
        vec!["present", "absent", "unknown_target", "unknown_encoded"]
    );
}
#[test]
fn resource_dependencies_follow_css_and_stop_cycles() {
    use seosmith::{
        graph_evidence::affected_pages,
        model::{Page, Relationship},
    };
    let relation = |url: &str| Relationship {
        url: url.into(),
        resource: true,
        ..Default::default()
    };
    let html = Page {
        url: "https://example.test/".into(),
        status: Some(200),
        content_type: "text/html".into(),
        relationships: vec![relation("https://example.test/a.css")],
        ..Default::default()
    };
    let css = Page {
        url: "https://example.test/a.css".into(),
        is_resource: true,
        relationships: vec![
            relation("https://example.test/image.png"),
            relation("https://example.test/a.css"),
        ],
        ..Default::default()
    };
    let affected = affected_pages(&[html, css]);
    assert!(affected["https://example.test/image.png"].contains("https://example.test/"));
}
#[test]
fn similarity_candidates_are_bounded_and_review_only() {
    use seosmith::graph_evidence::similarity_candidates;
    let pages = vec![
        page("/one", "<main>red ceramic mug with handle</main>"),
        page("/two", "<main>blue ceramic mug with handle</main>"),
        page("/short", "<main>Hello</main>"),
    ];
    let result = similarity_candidates(&pages, 3, 0.4, 10);
    assert_eq!(result.comparisons, 1);
    assert_eq!(result.candidates.len(), 1);
    assert!(result.candidates[0].review_only);
    assert!(!similarity_candidates(&pages, 3, 0.4, 0).complete);
}
#[test]
fn responsive_resources_preserve_source_attributes_without_selecting_a_candidate() {
    let p = page(
        "/",
        "<picture><source srcset='small.webp 1x, large.webp 2x' sizes='50vw'><img src='/fallback.png' loading='lazy' fetchpriority='low' width='320' height='200'></picture>",
    );
    assert_eq!(p.resource_references.len(), 2);
    assert_eq!(
        p.resource_references[0].raw_srcset.as_deref(),
        Some("small.webp 1x, large.webp 2x")
    );
    assert_eq!(p.resource_references[1].loading.as_deref(), Some("lazy"));
    assert_eq!(p.resource_references[1].width.as_deref(), Some("320"));
}
#[test]
fn canonical_variant_links_preserve_occurrence_and_do_not_validate_declaration() {
    use seosmith::{graph_evidence::canonical_variant_links, model::Page};
    let local = vec![page(
        "/",
        "<a href='/variant'>A</a><a href='/variant#two'>B</a>",
    )];
    let variant = Page {
        url: "https://example.test/variant".into(),
        status: Some(200),
        content_type: "text/html".into(),
        canonical: Some("https://outside.test/canonical".into()),
        ..Default::default()
    };
    let result = canonical_variant_links(&local, &[variant]);
    assert_eq!(result.len(), 2);
    assert_eq!(result[1].link_ordinal, 1);
    assert_eq!(
        result[0].declared_canonical,
        "https://outside.test/canonical"
    );
}
#[test]
fn duplicate_resource_groups_compare_decoded_bodies_not_urls() {
    use seosmith::graph_evidence::duplicate_resources;
    let headers = BTreeMap::new();
    let resources = vec![
        resource("https://example.test/a", &headers, b"same"),
        resource("https://example.test/b", &headers, b"same"),
        resource("https://example.test/c", &headers, b"other"),
    ];
    let groups = duplicate_resources(&resources);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].urls.len(), 2);
}
#[test]
fn byte_groups_label_decoded_measure_and_partial_collection() {
    use seosmith::graph_evidence::resource_byte_groups;
    let mut headers = BTreeMap::new();
    headers.insert("content-type".into(), "image/png; charset=binary".into());
    let resources = vec![
        resource("https://example.test/a", &headers, b"abc"),
        resource("https://example.test/b", &headers, b"12345"),
    ];
    let groups = resource_byte_groups(&resources, false);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].decoded_body_bytes, 8);
    assert_eq!(groups[0].origin.as_deref(), Some("https://example.test"));
    assert_eq!(groups[0].content_type.as_deref(), Some("image/png"));
    assert!(!groups[0].complete);
}
#[test]
fn graph_coverage_requires_known_reconciled_sources_and_no_omissions() {
    use seosmith::graph_evidence::{GraphCoverageInput, graph_coverage};
    let mut input = GraphCoverageInput {
        discovery_complete: Some(true),
        expected_pages: Some(2),
        observed_pages: 2,
        unavailable_pages: 0,
        truncated_pages: 0,
        omitted_records: Some(0),
        comparisons_complete: Some(true),
    };
    assert!(graph_coverage(&input).complete);
    input.discovery_complete = None;
    input.omitted_records = None;
    let partial = graph_coverage(&input);
    assert!(!partial.complete);
    assert!(partial.reasons.contains(&"discovery_incomplete_or_unknown"));
    assert!(partial.reasons.contains(&"records_omitted_or_unknown"));
    input.discovery_complete = Some(true);
    input.omitted_records = Some(0);
    input.expected_pages = Some(3);
    assert!(
        graph_coverage(&input)
            .reasons
            .contains(&"page_counts_unreconciled")
    );
    input.expected_pages = Some(2);
    input.truncated_pages = 3;
    assert!(
        graph_coverage(&input)
            .reasons
            .contains(&"truncation_counts_invalid")
    );
}
#[test]
fn similarity_clusters_are_connected_review_groups_and_preserve_partial_state() {
    use seosmith::graph_evidence::{CandidateSet, SimilarityCandidate, similarity_clusters};
    let edge = |a: &str, b: &str| SimilarityCandidate {
        first: a.into(),
        second: b.into(),
        similarity: 0.8,
        review_only: true,
    };
    let candidates = CandidateSet {
        candidates: vec![edge("a", "b"), edge("b", "c")],
        comparisons: 2,
        complete: false,
    };
    let (groups, complete) = similarity_clusters(&candidates);
    assert!(!complete);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].urls, vec!["a", "b", "c"]);
    assert!(groups[0].review_only);
}
