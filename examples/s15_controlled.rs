//! Offline benchmark binary using the real crawler and a deterministic transport.
use seosmith::{
    crawler,
    fetch::{Response, Transport},
    model::CrawlRequest,
    store::Store,
};
use std::{collections::BTreeMap, sync::Mutex};
use url::Url;
use uuid::Uuid;

struct Site(Mutex<Vec<String>>, bool);
impl Transport for Site {
    async fn get(&self, url: &Url) -> anyhow::Result<Response> {
        self.0.lock().unwrap().push(url.to_string());
        let labeled = self.1;
        let (kind, body) = match (labeled, url.path()) {
            (true, "/robots.txt") => ("text/plain", "User-agent: *\nDisallow:"),
            (true, "/sitemap.xml") => (
                "application/xml",
                "<urlset><url><loc>https://fixture.invalid/missing-title</loc></url><url><loc>https://fixture.invalid/has-title</loc></url><url><loc>https://fixture.invalid/missing-description</loc></url><url><loc>https://fixture.invalid/has-description</loc></url></urlset>",
            ),
            (true, "/missing-title") => (
                "text/html",
                include_str!("../fixtures/s15/missing-title.html"),
            ),
            (true, "/has-title") => ("text/html", include_str!("../fixtures/s15/has-title.html")),
            (true, "/missing-description") => (
                "text/html",
                include_str!("../fixtures/s15/missing-description.html"),
            ),
            (true, "/has-description") => (
                "text/html",
                include_str!("../fixtures/s15/has-description.html"),
            ),
            (_, "/robots.txt") => (
                "text/plain",
                "User-agent: *\nDisallow: /blocked\nSitemap: https://fixture.invalid/sitemap.xml",
            ),
            (_, "/sitemap.xml") => (
                "application/xml",
                "<urlset><url><loc>https://fixture.invalid/orphan</loc></url><url><loc>https://fixture.invalid/blocked</loc></url><url><loc>https://fixture.invalid/orphan</loc></url></urlset>",
            ),
            (_, "/") => (
                "text/html",
                "<html><head><title>Fixture seed document</title></head><body><a href='/linked'>link</a><a href='/linked'>duplicate</a><a href='/blocked'>blocked</a><a href='/over-budget'>budget</a></body></html>",
            ),
            _ => (
                "text/html",
                "<html><head><title>Fixture linked document</title></head><body>Fixture</body></html>",
            ),
        };
        Ok(Response {
            status: 200,
            headers: BTreeMap::from([("content-type".into(), kind.into())]),
            body: body.as_bytes().to_vec(),
            ttfb_ms: 0,
        })
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let db = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("database path required"))?;
    let mode = std::env::args().nth(2).unwrap_or_else(|| "budget".into());
    anyhow::ensure!(mode == "budget" || mode == "labeled", "invalid mode");
    let labeled = mode == "labeled";
    let store = Store::open(&db).await?;
    let request = CrawlRequest {
        request_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111")?,
        website_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222")?,
        binding_generation: 1,
        url: Url::parse("https://fixture.invalid/")?,
        max_pages: if labeled { 10 } else { 3 },
        max_resources: 0,
        max_duration_seconds: 20,
        delay_ms: 1000,
        check_external_links: false,
    };
    store.submit(&request).await?;
    store
        .claim()
        .await?
        .ok_or_else(|| anyhow::anyhow!("claim failed"))?;
    let site = Site(Mutex::new(Vec::new()), labeled);
    let report = crawler::crawl_with(&store, &request, &site).await?;
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"report": report, "fetch_attempts": *site.0.lock().unwrap()})
        )?
    );
    Ok(())
}
