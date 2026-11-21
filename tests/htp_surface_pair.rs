//! Controlled producer bytes for HTP surface regression verification.
use seosmith::{
    contract, crawler,
    fetch::{Response, Transport},
    model::CrawlRequest,
    store::Store,
};
use std::collections::BTreeMap;
use url::Url;
use uuid::Uuid;

struct Fixture {
    title: bool,
}
impl Transport for Fixture {
    fn get(&self, url: &Url) -> impl std::future::Future<Output = anyhow::Result<Response>> + Send {
        let (status, body) = match url.path() {
            "/robots.txt" | "/sitemap.xml" => (404, ""),
            _ if self.title => (
                200,
                "<html><head><title>Observed title</title></head><body>Fixture</body></html>",
            ),
            _ => (200, "<html><head></head><body>Fixture</body></html>"),
        };
        std::future::ready(Ok(Response {
            status,
            headers: BTreeMap::from([("content-type".into(), "text/html".into())]),
            raw_headers: None,
            body: body.as_bytes().to_vec(),
            ttfb_ms: 0,
        }))
    }
}

#[tokio::test]
#[ignore = "writes controlled producer reports to SEOSMITH_H15_PAIR_DIR"]
async fn export_producer_pair() {
    let output = std::path::PathBuf::from(
        std::env::var_os("SEOSMITH_H15_PAIR_DIR").expect("output directory required"),
    );
    std::fs::create_dir_all(&output).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("fixture.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let mut results = Vec::new();
    for (name, title, request_id) in [
        ("pass", true, "00000000-0000-4000-8000-000000000101"),
        ("fail", false, "00000000-0000-4000-8000-000000000102"),
    ] {
        let r = CrawlRequest {
            request_id: Uuid::parse_str(request_id).unwrap(),
            website_id: Uuid::parse_str("00000000-0000-4000-8000-000000000202").unwrap(),
            binding_generation: 7,
            url: Url::parse("https://example.com/").unwrap(),
            max_pages: 10,
            max_duration_seconds: 30,
            delay_ms: 1000,
            check_external_links: false,
            max_resources: 0,
        };
        store.submit(&r).await.unwrap();
        assert!(store.claim().await.unwrap().is_some());
        let mut report = crawler::crawl_with(&store, &r, &Fixture { title })
            .await
            .unwrap();
        assert!(report.coverage_complete);
        contract::enable_v2(&mut report).unwrap();
        let eval = report
            .evidence_contract
            .as_ref()
            .unwrap()
            .evaluations
            .iter()
            .find(|e| {
                e.rule == "missing_title"
                    && e.entity_id == contract::entity_id(r.url.as_str(), false)
            })
            .unwrap();
        assert!(matches!(
            (&eval.outcome, title),
            (contract::Outcome::Pass, true) | (contract::Outcome::Fail, false)
        ));
        results.push((
            eval.entity_id.clone(),
            eval.rule_version.clone(),
            eval.validator_version.clone(),
            report
                .evidence_contract
                .as_ref()
                .unwrap()
                .policy
                .signature
                .clone(),
        ));
        store.finish(&report).await.unwrap();
        let bytes = store.report(request_id).await.unwrap().unwrap();
        assert_eq!(bytes, serde_json::to_string(&report).unwrap());
        std::fs::write(output.join(format!("{name}.json")), bytes.as_bytes()).unwrap();
    }
    assert_eq!(results[0], results[1]);
}
