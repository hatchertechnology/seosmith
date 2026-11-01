use seosmith::{
    audit, crawler, extract,
    fetch::{Response, Transport},
    model::{CrawlRequest, Page},
    parse,
    store::Store,
};
use std::{collections::BTreeMap, sync::Mutex};
use url::Url;
use uuid::Uuid;
fn response(body: &str) -> Response {
    Response {
        status: 200,
        headers: BTreeMap::from([("content-type".into(), "text/html".into())]),
        body: body.as_bytes().to_vec(),
        ttfb_ms: 1,
    }
}
fn page(body: &str) -> Page {
    parse::parse(
        &Url::parse("https://example.com/").unwrap(),
        0,
        false,
        &response(body),
    )
}
#[test]
fn extracts_jsonld_graphs_errors_and_repeated_social_metadata() {
    let p = page(
        r#"<head>
    <script type="application/ld+json">{"@context":"https://schema.org","@graph":[{"@type":"LocalBusiness","name":"Client"}]}</script>
    <script type="APPLICATION/LD+JSON">[{"@type":"Product"}]</script>
    <script type="application/ld+json">{broken}</script>
    <meta property="og:title" content="Client &amp; Co"><meta property="og:image" content="one.png"><meta property="og:image" content="two.png">
    <meta name="twitter:card" content="summary_large_image"></head>"#,
    );
    assert_eq!(p.structured_data.len(), 3);
    assert_eq!(
        p.structured_data[0].data.as_ref().unwrap()["@graph"][0]["name"],
        "Client"
    );
    assert!(p.structured_data[1].data.as_ref().unwrap().is_array());
    assert!(p.structured_data[2].error.is_some());
    assert_eq!(p.open_graph["og:image"], vec!["one.png", "two.png"]);
    assert_eq!(p.open_graph["og:title"], vec!["Client & Co"]);
    assert_eq!(p.twitter["twitter:card"], vec!["summary_large_image"]);
    assert!(
        audit::analyze(&[p], &Url::parse("https://example.com").unwrap())
            .iter()
            .any(|f| f.rule == "invalid_json_ld")
    );
}
#[test]
fn microdata_preserves_nested_scopes_multiple_values_and_itemref() {
    let p = page(
        r#"<div itemscope itemtype="https://schema.org/LocalBusiness" itemid="/business" itemref="extra"><span itemprop="name alternateName">Client</span><a itemprop="url" href="/contact">Contact</a><div itemprop="address" itemscope itemtype="https://schema.org/PostalAddress"><span itemprop="streetAddress">123 Main</span></div></div><div id="extra"><meta itemprop="telephone" content="555-0100"></div>"#,
    );
    assert_eq!(p.structured_data.len(), 1);
    let data = p.structured_data[0].data.as_ref().unwrap();
    assert_eq!(data["id"], "https://example.com/business");
    assert_eq!(data["properties"]["url"][0], "https://example.com/contact");
    assert_eq!(data["properties"]["name"][0], "Client");
    assert_eq!(data["properties"]["telephone"][0], "555-0100");
    assert_eq!(
        data["properties"]["address"][0]["properties"]["streetAddress"][0],
        "123 Main"
    );
    assert!(data["properties"].get("streetAddress").is_none());
}
#[test]
fn dependencies_refresh_and_link_relations_are_typed() {
    let p = page(
        r#"<head><base href="/assets/"><meta http-equiv="Refresh" content="0; URL='../destination'"><link rel="stylesheet" href="site.css"><link rel="preload" href="font.woff2"><script src="app.js"></script><style>.hero{background:url('hero.png')}</style></head><body><a href="/sponsored" rel="SPONSORED ugc">Ad</a><img src="small.jpg" srcset="medium.jpg 600w, large.jpg 1200w"><picture><source srcset="wide.webp 2x"><img src="fallback.jpg"></picture><video poster="poster.jpg"><source src="clip.mp4"></video><iframe src="/embed"></iframe><div style="background:url(inline.png)"></div></body>"#,
    );
    assert!(!p.links[0].nofollow);
    assert!(p.links[0].sponsored && p.links[0].ugc);
    assert_eq!(
        p.refresh_target.as_deref(),
        Some("https://example.com/destination")
    );
    for (kind, path) in [
        ("stylesheet", "site.css"),
        ("script", "app.js"),
        ("image_candidate", "large.jpg"),
        ("image_candidate", "wide.webp"),
        ("video_poster", "poster.jpg"),
        ("source", "clip.mp4"),
        ("css_url", "hero.png"),
        ("css_url", "inline.png"),
    ] {
        assert!(
            p.relationships
                .iter()
                .any(|r| r.kind == kind && r.url == format!("https://example.com/assets/{path}")),
            "{kind} {path}"
        );
    }
    let urls = extract::css_urls(
        "@import 'base.css'; @media screen { a {background: url(\"a,b.png\")} } /* url(fake.png) */",
    );
    assert_eq!(urls, vec!["base.css", "a,b.png"]);
    assert_eq!(
        extract::srcset("data:image/png;base64,abc 1x, /two.png 2x")[1].0,
        "/two.png"
    );
}
#[test]
fn charset_header_relations_and_xhtml_are_supported() {
    let mut r = response("<html><head><title>Caf\u{00e9}</title></head><body></body></html>");
    r.body = b"<html><head><title>Caf\xe9</title></head><body></body></html>".to_vec();
    r.headers.insert(
        "content-type".into(),
        "application/xhtml+xml; charset=windows-1252".into(),
    );
    r.headers.insert("content-language".into(), "fr".into());
    r.headers.insert("link".into(),"</canonical?a=1,2>; rel=\"canonical\", </fr>; rel=\"alternate\"; hreflang=\"fr\"; title=\"French, page\"".into());
    let p = parse::parse(&Url::parse("https://example.com/").unwrap(), 0, false, &r);
    assert_eq!(p.title, "Caf\u{00e9}");
    assert!(p.html());
    assert_eq!(p.language, "fr");
    assert_eq!(p.language_source, "http_header");
    assert_eq!(
        p.canonical.as_deref(),
        Some("https://example.com/canonical?a=1,2")
    );
    assert_eq!(p.alternates[0].language, "fr");
    let (decoded, _, _) = extract::decode(
        b"<meta charset=windows-1252><title>Caf\xe9</title>",
        "text/html",
    );
    assert!(decoded.contains("Caf\u{00e9}"));
}
#[test]
fn findings_retain_every_broken_target_and_validate_language_tags() {
    let p = page(
        "<html lang='en_US'><head><link rel='alternate' hreflang='not_a_tag' href='/fr'></head><body><a href='/one'>One</a><a href='/two'>Two</a></body></html>",
    );
    let targets = ["one", "two"].map(|s| Page {
        url: format!("https://example.com/{s}"),
        status: Some(404),
        ..Page::default()
    });
    let findings = audit::analyze(
        &[vec![p], targets.to_vec()].concat(),
        &Url::parse("https://example.com/").unwrap(),
    );
    let broken = findings
        .iter()
        .find(|f| f.rule == "broken_internal_link")
        .unwrap();
    assert_eq!(broken.evidence_items.len(), 2);
    assert_eq!(broken.details["observation_count"], 2);
    assert!(findings.iter().any(|f| f.rule == "invalid_language"));
    assert!(findings.iter().any(|f| f.rule == "invalid_hreflang"));
}
#[test]
fn extraction_limits_are_explicit_and_old_checkpoints_deserialize() {
    let oversized = format!(
        "<script type='application/ld+json'>{{\"name\":\"{}\"}}</script>",
        "x".repeat(66_000)
    );
    let p = page(&oversized);
    assert!(p.extraction_truncated);
    assert!(p.flags.contains(&"extraction_limit".into()));
    let old = serde_json::to_value(Page::default()).unwrap();
    let mut old = old.as_object().unwrap().clone();
    for key in [
        "relationships",
        "structured_data",
        "open_graph",
        "twitter",
        "is_resource",
        "encoding",
        "language_source",
        "refresh_delay",
        "refresh_target",
        "extraction_truncated",
    ] {
        old.remove(key);
    }
    assert!(
        serde_json::from_value::<Page>(old.into())
            .unwrap()
            .relationships
            .is_empty()
    );
}
struct Website {
    calls: Mutex<Vec<String>>,
    include_blocked: bool,
}
impl Transport for Website {
    fn get(&self, u: &Url) -> impl std::future::Future<Output = anyhow::Result<Response>> + Send {
        self.calls.lock().unwrap().push(u.to_string());
        let mut r = match u.path() {
            "/robots.txt" => response("User-agent: *\nDisallow: /private.png"),
            "/sitemap.xml" | "/missing.png" => Response {
                status: 404,
                ..response("")
            },
            "/" => {
                let body = r#"<head><link rel='canonical' href='/canonical'><link rel='alternate' hreflang='fr' href='/fr'><link rel='stylesheet' href='https://cdn.example/site.css'><meta http-equiv='refresh' content='1;url=/about'><meta property='og:title' content='Client'><meta property='og:image' content='/one.webp'><meta property='og:image' content='/two.webp'><meta name='twitter:card' content='summary'></head><body><img src='/private.png'><a href='/ad' rel='sponsored'>Ad</a><script type='application/ld+json'>{"@context":"https://schema.org","@type":"WebPage"}</script><div itemscope itemtype='https://schema.org/LocalBusiness'><span itemprop='name'>Client</span><div itemprop='address' itemscope itemtype='https://schema.org/PostalAddress'><span itemprop='streetAddress'>123 Main</span></div></div></body>"#;
                response(&if self.include_blocked {
                    body.to_owned()
                } else {
                    body.replace("<img src='/private.png'>", "")
                })
            }
            "/site.css" => {
                let mut r = response("@import 'base.css';body{background:url(missing.png)}");
                r.headers.insert("content-type".into(), "text/css".into());
                r
            }
            "/base.css" => {
                let mut r = response("body{color:red}");
                r.headers.insert("content-type".into(), "text/css".into());
                r
            }
            "/fr" => response(
                "<html lang='fr'><head><link rel='alternate' hreflang='en' href='/'></head><body>French</body></html>",
            ),
            _ => response("<html><head><title>Destination</title></head><body></body></html>"),
        };
        if u.path() == "/robots.txt" {
            r.headers.clear();
        }
        std::future::ready(Ok(r))
    }
}
async fn crawl(max_resources: usize) -> (seosmith::model::Report, Website) {
    crawl_options(max_resources, true).await
}
async fn crawl_options(
    max_resources: usize,
    include_blocked: bool,
) -> (seosmith::model::Report, Website) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("audit.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let r = CrawlRequest {
        request_id: Uuid::new_v4(),
        website_id: Uuid::new_v4(),
        binding_generation: 1,
        url: Url::parse("https://example.com/").unwrap(),
        max_pages: 4,
        max_resources,
        max_duration_seconds: 60,
        delay_ms: 1000,
        check_external_links: false,
    };
    store.submit(&r).await.unwrap();
    store.claim().await.unwrap();
    let site = Website {
        calls: Mutex::new(vec![]),
        include_blocked,
    };
    let report = crawler::crawl_with(&store, &r, &site).await.unwrap();
    store.finish(&report).await.unwrap();
    let saved: seosmith::model::Report = serde_json::from_str(
        &store
            .report(&r.request_id.to_string())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.pages.len(), report.pages.len());
    (report, site)
}
#[tokio::test]
async fn crawls_indirect_targets_cdn_css_and_dependencies_without_spending_page_budget() {
    let (report, site) = crawl(10).await;
    assert!(report.coverage_complete, "{}", report.stop_reason);
    assert_eq!(report.pages.iter().filter(|p| !p.is_resource).count(), 4);
    for expected in [
        "https://example.com/canonical",
        "https://example.com/about",
        "https://example.com/fr",
        "https://cdn.example/site.css",
        "https://cdn.example/base.css",
        "https://cdn.example/missing.png",
    ] {
        assert!(
            site.calls.lock().unwrap().contains(&expected.into()),
            "{expected}"
        );
    }
    assert!(
        !site
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("schema.org") || u.ends_with("/ad") || u.ends_with("/private.png"))
    );
    let root = report
        .pages
        .iter()
        .find(|p| p.url == "https://example.com/")
        .unwrap();
    assert_eq!(
        root.relationships
            .iter()
            .find(|r| r.url.ends_with("/private.png"))
            .unwrap()
            .state,
        "blocked"
    );
    assert_eq!(
        root.relationships
            .iter()
            .find(|r| r.kind == "canonical")
            .unwrap()
            .state,
        "checked"
    );
    assert!(
        report
            .pages
            .iter()
            .any(|p| p.is_resource && p.url.ends_with("missing.png"))
    );
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.rule == "broken_resource" && f.url == "https://cdn.example/site.css")
    );
}
#[tokio::test]
async fn resource_budget_marks_partial_and_leaves_targets_unknown() {
    let (report, _) = crawl(0).await;
    assert!(!report.coverage_complete);
    assert_eq!(report.stop_reason, "resource_limit");
    assert_eq!(report.pages.len(), 4);
    assert!(
        report.pages[0]
            .relationships
            .iter()
            .any(|r| r.resource && r.state == "not_fetched")
    );
}

#[test]
fn css_limits_and_refresh_edge_cases_do_not_claim_full_extraction() {
    let css = "a:0;".repeat(6000);
    assert!(extract::css_observations(&css).1);
    let mut r = response(&css);
    r.headers.insert("content-type".into(), "text/css".into());
    let p = parse::parse(
        &Url::parse("https://example.com/site.css").unwrap(),
        0,
        false,
        &r,
    );
    assert!(p.extraction_truncated);
    assert!(p.flags.contains(&"extraction_limit".into()));
    for refresh in ["0;x", "0;u", "nan;url=/next", "-1;url=/next"] {
        let p = page(&format!("<meta http-equiv='refresh' content='{refresh}'>"));
        assert!(p.refresh_target.is_none());
    }
}

#[test]
fn refresh_redirects_participate_in_chain_and_loop_analysis() {
    let a = page("<meta http-equiv='refresh' content='0;url=/next'>");
    let b = Page {
        url: "https://example.com/next".into(),
        status: Some(301),
        redirect: Some("https://example.com/".into()),
        ..Page::default()
    };
    assert!(a.redirect.is_none());
    let findings = audit::analyze(&[a, b], &Url::parse("https://example.com/").unwrap());
    assert_eq!(
        findings
            .iter()
            .filter(|f| f.rule == "redirect_loop")
            .count(),
        2
    );
    assert_eq!(
        findings
            .iter()
            .filter(|f| f.rule == "redirect_chain")
            .count(),
        2
    );
}

/// Generate real collector reports for the opt-in cross-repository HTP check.
#[tokio::test]
#[ignore = "run with SEOSMITH_CONTRACT_DIR for local HTP integration verification"]
async fn export_htp_contract_reports() {
    let output = std::path::PathBuf::from(
        std::env::var_os("SEOSMITH_CONTRACT_DIR").expect("SEOSMITH_CONTRACT_DIR required"),
    );
    std::fs::create_dir_all(&output).unwrap();
    for (name, resources, blocked, complete) in [
        ("complete", 10, false, true),
        ("resource_limited", 0, false, false),
        ("robots_excluded", 10, true, true),
    ] {
        let (report, _) = crawl_options(resources, blocked).await;
        assert_eq!(report.coverage_complete, complete);
        std::fs::write(
            output.join(format!("{name}.json")),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn binary_resources_are_not_decoded_as_html_text() {
    let mut r = response("");
    r.headers.insert("content-type".into(), "image/png".into());
    r.body = vec![0xff, 0x89, 0, 0xfe];
    let p = parse::parse(
        &Url::parse("https://example.com/image.png").unwrap(),
        0,
        false,
        &r,
    );
    assert_eq!(p.bytes, 4);
    assert_eq!(p.encoding, "");
    assert!(!p.flags.contains(&"encoding_errors".into()));
    assert!(p.relationships.is_empty());
}
