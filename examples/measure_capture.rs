//! A11 / N-8 measurement: one polite worker-mode capture of a site.
//!
//! Crawls `<seed>` with `CaptureMode::Wire2` into a temporary store, then reports
//! the graph collector's serialized bytes (p50 / p95 / max) and how many pages lost
//! the graph or another collector to `collector_byte_limit`. It then re-fetches each
//! crawled HTML page once and times only `local_checkpoint::capture_with`, so the
//! reported capture time (p95 per page) excludes network time.
//!
//! ```text
//! cargo run --locked --example measure_capture -- https://www.example.com/ [max_pages=57] [delay_ms=1000]
//! ```
//! Output is one JSON object on stdout; progress goes to stderr.
use anyhow::{Context, Result, bail};
use seosmith::{
    crawler,
    families::types::CaptureMode,
    fetch::{Fetcher, Transport},
    local_checkpoint,
    model::{CrawlRequest, Report},
    store::Store,
};
use serde_json::{Value, json};
use sqlx::Row;
use std::time::{Duration, Instant};
use url::Url;

/// Nearest-rank percentile (`percent` in 1..=100) of a sorted slice.
fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * percent).div_ceil(100);
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn summary(mut values: Vec<u64>) -> Value {
    values.sort_unstable();
    json!({"count": values.len(), "p50": percentile(&values, 50),
        "p95": percentile(&values, 95), "max": values.last().copied().unwrap_or(0)})
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(seed) = args.next() else {
        bail!("usage: measure_capture <seed-url> [max_pages=57] [delay_ms=1000]");
    };
    let max_pages: usize = args.next().map_or(Ok(57), |v| v.parse())?;
    let delay_ms: u64 = args.next().map_or(Ok(1000), |v| v.parse())?;
    let seed = Url::parse(&seed)?;

    // A valid request shape comes from the committed legacy fixture.
    let legacy: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let request = CrawlRequest {
        request_id: uuid::Uuid::new_v4(),
        url: seed,
        max_pages,
        max_resources: 0,
        max_duration_seconds: 900,
        delay_ms,
        check_external_links: false,
        ..legacy.request
    };
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("measure.sqlite").to_str().context("path")?).await?;
    store.submit(&request).await?;
    store.claim_with(CaptureMode::Wire2).await?;
    eprintln!("crawling up to {max_pages} pages at {delay_ms} ms ...");
    let report = crawler::crawl_captured(&store, &request, CaptureMode::Wire2).await?;
    let id = request.request_id.to_string();

    let rows = sqlx::query("SELECT evidence FROM local_page_evidence WHERE job_id=?")
        .bind(&id)
        .fetch_all(&store.0)
        .await?;
    let mut graph_bytes = Vec::new();
    let mut limit_hits = std::collections::BTreeMap::<String, u64>::new();
    for row in &rows {
        let value: Value = serde_json::from_str(&row.get::<String, _>("evidence"))?;
        for c in value["capture"]["collectors"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let name = c["collector"].as_str().unwrap_or("?");
            if name == "graph" && c["data"].is_object() {
                graph_bytes.push(serde_json::to_vec(&c["data"])?.len() as u64);
            }
            if c["reason"] == "collector_byte_limit" || c["reason"] == "run_byte_limit" {
                *limit_hits
                    .entry(format!("{name}:{}", c["reason"].as_str().unwrap_or("")))
                    .or_default() += 1;
            }
        }
    }

    eprintln!("timing capture over {} pages ...", report.pages.len());
    let fetcher = Fetcher::default();
    let mut micros = Vec::new();
    for page in report.pages.iter().filter(|p| p.html() && !p.is_resource) {
        let url = Url::parse(&page.url)?;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let Ok(response) = fetcher.get(&url).await else {
            continue;
        };
        let started = Instant::now();
        let evidence =
            local_checkpoint::capture_with(&url, page, Some(&response), CaptureMode::Wire2);
        micros.push(u64::try_from(started.elapsed().as_micros())?);
        std::hint::black_box(evidence);
    }
    let pages = report.pages.len();
    let graph_unavailable = limit_hits
        .get("graph:collector_byte_limit")
        .copied()
        .unwrap_or(0);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "seed": request.url.as_str(),
            "pages_crawled": pages,
            "evidence_rows": rows.len(),
            "graph_bytes": summary(graph_bytes),
            "graph_collector_byte_limit_pages": graph_unavailable,
            "graph_collector_byte_limit_per_mille_of_pages": if pages == 0 { 0 }
                else { graph_unavailable * 1000 / pages as u64 },
            "capture_limit_hits": limit_hits,
            "capture_micros_per_html_page": summary(micros),
            "graph_limit_bytes": 64 * 1024,
        }))?
    );
    Ok(())
}
