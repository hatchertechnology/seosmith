use seosmith::graph_evidence::{extract, graph};
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
    let nodes = graph("https://example.test/", &[home, a, sitemap_only], true).unwrap();
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
    )
    .unwrap();
    assert_eq!(nodes[1].reachability, "unknown");
}
#[test]
fn fragment_absence_requires_observed_target() {
    use seosmith::graph_evidence::fragment_checks;
    let home = page(
        "/",
        "<a href='/a#yes'>yes</a><a href='/a#no'>no</a><a href='/missing#x'>x</a><a href='/a#%79es'>encoded</a>",
    );
    let target = page("/a", "<div id=yes></div>");
    let checks = fragment_checks(&[home, target]).unwrap();
    assert_eq!(
        checks.iter().map(|c| c.state).collect::<Vec<_>>(),
        vec!["present", "absent", "unknown_target", "unknown_encoded"]
    );
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
fn missing_seed_nofollow_and_truncation_remain_conservative() {
    let other = page("/other", "");
    assert_eq!(
        graph("https://example.test/", &[other], true).unwrap()[0].reachability,
        "unknown"
    );
    let home = page("/", "<a rel=nofollow href='/other'>No follow</a>");
    let nodes = graph("https://example.test/", &[home, page("/other", "")], true).unwrap();
    assert_eq!(nodes[1].unique_inlinks, 1);
    assert_eq!(nodes[1].navigation_distance, None);
    let html = format!("<a href='/{}'>long</a>", "x".repeat(2050));
    assert!(!page("/", &html).extraction_complete);
}

#[test]
fn truncated_target_cannot_prove_missing_fragment() {
    use seosmith::graph_evidence::fragment_checks;
    let home = page("/", "<a href='/target#missing'>link</a>");
    let html = "<i id='same'></i>".repeat(10001);
    let target = page("/target", &html);
    assert_eq!(
        fragment_checks(&[home, target]).unwrap()[0].state,
        "unknown_truncated"
    );
}

#[test]
fn fragment_targets_honor_anchor_names_and_top_fallback() {
    use seosmith::graph_evidence::fragment_checks;
    let home = page(
        "/",
        "<a href='/a#bogus'>a</a><a href='/a#named'>b</a><a href='/a#TOP'>c</a><a href='/b#top'>d</a>",
    );
    let a = page(
        "/a",
        "<input id=field name=bogus><a id=other name=named></a>",
    );
    let b = page("/b", "<div id=top></div>");
    let checks = fragment_checks(&[home, a, b]).unwrap();
    assert_eq!(
        checks.iter().map(|c| c.state).collect::<Vec<_>>(),
        ["absent", "present", "top", "present"]
    );
    let home = page("/", "<a href='/a#top'>a</a>");
    let a = page("/a", &"<i id=same></i>".repeat(10001));
    assert_eq!(
        fragment_checks(&[home, a]).unwrap()[0].state,
        "unknown_truncated"
    );
}

#[test]
fn duplicate_page_urls_are_rejected_in_either_order() {
    use seosmith::graph_evidence::{DuplicatePageUrl, fragment_checks};
    let linked = page("/", "<a href='/a#x'>link</a>");
    let empty = page("/", "");
    let target = page("/a", "<i id=x></i>");
    for pages in [
        [linked.clone(), empty.clone(), target.clone()],
        [empty.clone(), linked.clone(), target.clone()],
    ] {
        assert_eq!(
            graph("https://example.test/", &pages, true).unwrap_err(),
            DuplicatePageUrl("https://example.test/".into())
        );
        assert_eq!(
            fragment_checks(&pages).unwrap_err(),
            DuplicatePageUrl("https://example.test/".into())
        );
    }
    let home = page("/", "<a href='/a#x'>link</a>");
    let absent = page("/a", "");
    for pages in [
        [home.clone(), target.clone(), absent.clone()],
        [home.clone(), absent.clone(), target.clone()],
    ] {
        assert_eq!(
            fragment_checks(&pages).unwrap_err(),
            DuplicatePageUrl("https://example.test/a".into())
        );
    }
}
