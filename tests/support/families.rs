//! A crawled wire2 job plus the §3.2 invariant checker, shared by the families tests.
#![allow(dead_code)]
use seosmith::{
    crawler,
    families::types::{CaptureMode, FamilyKey, FamilySection, FamilySet, Outcome, SectionState},
    model::Report,
    store::Store,
};

#[path = "site.rs"]
pub mod site;
pub use site::{Site, request, unpaced};

pub struct Job {
    pub _dir: tempfile::TempDir,
    pub store: Store,
    pub id: String,
    pub report: Report,
}

pub async fn crawl(site: &Site, max_pages: usize, max_resources: usize, mode: CaptureMode) -> Job {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("s.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let r = request(max_pages, max_resources);
    store.submit(&r).await.unwrap();
    store.claim_with(mode).await.unwrap().unwrap();
    let report = crawler::crawl_captured_with(&store, &unpaced(&r), mode, site)
        .await
        .unwrap();
    Job {
        _dir: dir,
        store,
        id: r.request_id.to_string(),
        report,
    }
}

pub fn rich_site() -> Site {
    Site::default()
        .robots("User-agent: *\nDisallow: /private\nSitemap: https://site.invalid/sitemap.xml\n")
        .raw("https://site.invalid/sitemap.xml", 200, &[("content-type", "application/xml")],
            b"<urlset><url><loc>https://site.invalid/</loc></url><url><loc>https://site.invalid/a</loc></url><url><loc>https://site.invalid/orphan</loc></url></urlset>")
        .html("https://site.invalid/", "<html><head><title>Home</title><link rel='canonical' href='https://site.invalid/'><link rel='stylesheet' href='/s.css'><script type='application/ld+json'>{\"@context\":\"https://schema.org\",\"@type\":\"Product\",\"name\":\"x\"}</script></head><body><h1>Home</h1><a href='/a'>a</a><a href='/dup'>d</a><a href='/dup2'>d2</a><a href='/private'>p</a><a href='/a#missing'>f</a><img src='/i.png'></body></html>")
        .html("https://site.invalid/a", "<html><head><title>A</title><link rel='canonical' href='https://site.invalid/'><meta name='robots' content='noindex'><link rel='alternate' hreflang='fr' href='https://site.invalid/fr'></head><body><h1>A</h1></body></html>")
        .html("https://site.invalid/dup", "<html><head><title>Same</title></head><body><h1>Same</h1><p>identical body</p></body></html>")
        .html("https://site.invalid/dup2", "<html><head><title>Same</title></head><body><h1>Same</h1><p>identical body</p></body></html>")
        .html("https://site.invalid/private", "<html><head><title>P</title></head><body>P</body></html>")
        .raw("https://site.invalid/s.css", 200, &[("content-type", "text/css"), ("content-encoding", "identity")], b"body{color:red}")
        .raw("https://site.invalid/i.png", 200, &[("content-type", "image/png"), ("cache-control", "no-store")], &vec![0u8; 5000])
}

pub fn section(set: &FamilySet, key: FamilyKey) -> &FamilySection {
    &set.0[&key]
}

/// Design 3.2 invariants 1-4, 6 and 7, plus locator binding against the report.
pub fn check_invariants(set: &FamilySet, report: &Report) {
    assert_eq!(set.0.len(), 6, "all six keys are present");
    for (key, s) in &set.0 {
        let name = key.as_str();
        assert_eq!(
            s.items_total,
            s.items.len() as u64 + s.omitted_items,
            "{name}: invariant 1"
        );
        assert_eq!(s.truncated, s.omitted_items > 0, "{name}: invariant 2");
        if s.truncated {
            assert!(
                matches!(
                    s.omitted_reason.as_deref(),
                    Some(
                        "family_item_limit"
                            | "family_byte_budget"
                            | "report_byte_budget"
                            | "analysis_deadline"
                    )
                ),
                "{name}: omitted_reason {:?}",
                s.omitted_reason
            );
        }
        if s.state == SectionState::Complete {
            assert_eq!(s.coverage.unknown, 0, "{name}: complete means no unknown");
            assert!(!s.truncated, "{name}: complete means not truncated");
        }
        if s.state == SectionState::Unavailable {
            assert!(
                s.items.is_empty() && s.reason.is_some(),
                "{name}: invariant 3"
            );
        }
        assert_eq!(
            s.coverage.unknown_reasons.values().sum::<u64>(),
            s.coverage.unknown,
            "{name}: invariant 4"
        );
        let unknown_items = s
            .items
            .iter()
            .filter(|i| i.outcome == Outcome::Unknown)
            .count() as u64;
        assert!(
            unknown_items <= s.coverage.unknown,
            "{name}: every unknown item is counted"
        );
        let code = |c: &str| {
            !c.is_empty()
                && c.len() <= 64
                && c.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        for reason in s.coverage.unknown_reasons.keys() {
            assert!(code(reason), "{name}: reason code {reason}");
        }
        assert!(s.items.len() as u64 <= s.max_items);
        let mut last = None;
        for item in &s.items {
            assert!(code(&item.kind), "{name}: kind {}", item.kind);
            if item.outcome == Outcome::Unknown {
                assert!(
                    item.reason.as_deref().is_some_and(code),
                    "{name}: unknown needs a code"
                );
            }
            if let Some(reason) = &item.reason {
                assert!(code(reason));
            }
            let facts = serde_json::to_vec(&item.facts).unwrap();
            assert!(facts.len() <= 4096, "{name}: facts bytes {}", facts.len());
            assert!(!String::from_utf8_lossy(&facts).contains("\\u0000"));
            // Locator binding.
            let (Some(locator), Some(entity)) = (&item.locator, &item.entity_id) else {
                panic!("{name}: every item here is bound to a page");
            };
            let n: usize = locator.strip_prefix("/pages/").unwrap().parse().unwrap();
            let page = &report.pages[n];
            assert_eq!(
                entity,
                &seosmith::contract::entity_id(&page.url, page.is_resource),
                "{name}: entity binding"
            );
            // Deterministic order.
            let key_now = (item.outcome, item.locator.clone(), item.kind.clone());
            if let Some(prev) = &last {
                assert!(prev <= &key_now, "{name}: order");
            }
            last = Some(key_now);
        }
        assert!(serde_json::to_vec(&s.summary).unwrap().len() <= 8192);
    }
}
