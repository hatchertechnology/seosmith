//! WP8b-3: the real `serve` worker (`process_jobs`) with `--policy-wire-v2` on and off.
//!
//! Design §7 (Wire-2 row): "`process_jobs` passes `Wire2` with the flag on and
//! `None` with it off." This drives the production worker binary offline
//! (`FixtureTransport`, as the 8b-1 `crawl_failure_codes` serve tests do) and
//! reads the observable results from the job row:
//!
//! * flag on: `jobs.capture_mode = 'wire2'` (so `claim_with(Wire2)` ran), captures
//!   in `local_page_evidence` (so `crawl_captured_with` captured), a stored
//!   `s14-wire-2` report with six families built from those captures;
//! * flag off: `jobs.capture_mode = 'none'`, no captures, no `evidence_families`,
//!   and a report byte-identical (wall-clock fields aside) to the one the
//!   unchanged wire-1 pipeline (`crawl_captured_with(None)` then `generate_wire_v1`)
//!   produces in-process over the same fixtures.
#![cfg(all(feature = "local-cli-fixtures", feature = "local-e2e"))]
use anyhow::{Context, Result};
use seosmith::{
    crawler,
    families::types::CaptureMode,
    fetch::{Response, Transport, extract_headers},
    finding_coverage_prototype::generate_wire_v1,
    model::CrawlRequest,
    policy::AdvisoryPolicy,
    store::Store,
};
use serde_json::Value;
use sqlx::sqlite::SqliteConnectOptions;
use std::{
    collections::BTreeMap,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const API_TOKEN: &str = "test-api-token-0123456789abcdef0123456789";
const INGEST_TOKEN: &str = "test-ingest-token-0123456789abcdef012345";
const HOST: &str = "https://wire2.example";

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

fn entry(status: u16, content_type: &str, body: &str) -> Value {
    serde_json::json!({"status": status, "headers": {"content-type": content_type}, "body": body})
}

/// A small site: a home page, two linked pages, one stylesheet, one external link.
fn fixtures() -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    map.insert(format!("{HOST}/robots.txt"), entry(404, "text/plain", ""));
    map.insert(format!("{HOST}/sitemap.xml"), entry(404, "text/plain", ""));
    map.insert(
        format!("{HOST}/"),
        entry(
            200,
            "text/html",
            "<html lang=\"en\"><head><title>Home page of the wire two site</title>\
             <link rel=\"stylesheet\" href=\"/assets/site.css\"></head><body><h1>Home</h1>\
             <a href=\"/a\">a</a><a href=\"/b\">b</a><a href=\"https://other.example/x\">x</a></body></html>",
        ),
    );
    map.insert(
        format!("{HOST}/a"),
        entry(
            200,
            "text/html",
            "<html lang=\"en\"><head><title>A page about the first thing</title></head><body><h1>A</h1></body></html>",
        ),
    );
    map.insert(
        format!("{HOST}/b"),
        entry(
            200,
            "text/html",
            "<html lang=\"en\"><head><title></title></head><body><a href=\"/a\">a</a></body></html>",
        ),
    );
    map.insert(
        format!("{HOST}/assets/site.css"),
        entry(200, "text/css", "body{}"),
    );
    map
}

/// The same lookup `main.rs` `FixtureTransport` performs, over the same JSON.
struct Mirror(BTreeMap<String, Value>);
impl Transport for Mirror {
    #[allow(clippy::unused_async_trait_impl)]
    async fn get(&self, url: &url::Url) -> Result<Response> {
        let f = self.0.get(url.as_str()).context("fixture URL missing")?;
        let mut fields = reqwest::header::HeaderMap::new();
        for (k, v) in f["headers"].as_object().unwrap() {
            fields.append(
                reqwest::header::HeaderName::from_bytes(k.as_bytes())?,
                reqwest::header::HeaderValue::from_bytes(v.as_str().unwrap().as_bytes())?,
            );
        }
        let (headers, occurrences) = extract_headers(&fields);
        Ok(Response {
            status: u16::try_from(f["status"].as_u64().unwrap())?,
            headers,
            local_http_occurrences: Some(occurrences),
            body: f["body"].as_str().unwrap().as_bytes().to_vec(),
            ttfb_ms: 0,
        })
    }
}

fn request(id: u128) -> Result<CrawlRequest> {
    Ok(serde_json::from_value(serde_json::json!({
        "request_id": uuid::Uuid::from_u128(id),
        "website_id": uuid::Uuid::from_u128(id + 1),
        "binding_generation": 1,
        "url": format!("{HOST}/"),
        "max_pages": 10,
        "max_duration_seconds": 3600,
        "delay_ms": 1000,
        "check_external_links": false,
        "max_resources": 5
    }))?)
}

struct Row {
    state: String,
    error: Option<String>,
    capture_mode: Option<String>,
    report: Option<String>,
    captures: i64,
}

/// Runs `serve` over the fixtures with one queued job and returns its settled row.
async fn serve_one(wire2: bool) -> Result<Row> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("db.sqlite");
    std::fs::write(
        dir.path().join("responses.json"),
        serde_json::to_vec(&fixtures())?,
    )?;
    let store = Store::open(db.to_str().unwrap()).await?;
    let r = request(0x7700)?;
    store.submit(&r).await?;
    drop(store);
    let id = r.request_id.to_string();

    let stderr_path = dir.path().join("serve.stderr");
    let port = free_port()?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_seosmith"));
    cmd.env_clear()
        .env("SEOSMITH_LOCAL_FIXTURES", dir.path().join("responses.json"))
        .env("SEOSMITH_API_TOKEN", API_TOKEN)
        .env("HTP_INGEST_URL", "http://127.0.0.1:1/ingest")
        .env("HTP_INGEST_TOKEN", INGEST_TOKEN)
        .args(["--database", db.to_str().unwrap()])
        .args([
            "--advisory-policy",
            "fixtures/policies/production-default.json",
        ])
        .arg("--policy-wire-v1");
    if wire2 {
        cmd.arg("--policy-wire-v2");
    }
    let child = cmd
        .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
        .stderr(std::fs::File::create(&stderr_path)?)
        .stdout(Stdio::null())
        .spawn()?;
    let _worker = Worker(child);
    let pool = loop {
        if let Ok(pool) = sqlx::SqlitePool::connect_with(
            SqliteConnectOptions::new().filename(&db).read_only(true),
        )
        .await
        {
            break pool;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let started = Instant::now();
    loop {
        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT state,error FROM jobs WHERE id=?")
                .bind(&id)
                .fetch_all(&pool)
                .await?;
        if let Some((state, error)) = rows.into_iter().next()
            && !matches!(state.as_str(), "queued" | "running")
        {
            let (capture_mode, report): (Option<String>, Option<String>) =
                sqlx::query_as("SELECT capture_mode,report FROM jobs WHERE id=?")
                    .bind(&id)
                    .fetch_one(&pool)
                    .await?;
            let captures: i64 =
                sqlx::query_scalar("SELECT count(*) FROM local_page_evidence WHERE job_id=?")
                    .bind(&id)
                    .fetch_one(&pool)
                    .await?;
            return Ok(Row {
                state,
                error,
                capture_mode,
                report,
                captures,
            });
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(120),
            "worker did not settle:\n{}",
            std::fs::read_to_string(&stderr_path).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// The ingest URL is a closed port, so delivery may already have recorded
/// `htp_delivery_failed` on a completed job: the state shows the worker finalized.
fn assert_completed(row: &Row) {
    assert!(
        row.state == "completed"
            && matches!(row.error.as_deref(), None | Some("htp_delivery_failed")),
        "{} {:?}",
        row.state,
        row.error
    );
}

/// Replaces the wall-clock fields (`started_at`, `finished_at`, wherever they
/// occur) so two runs of the same crawl compare equal.
fn without_clock(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if k == "started_at" || k == "finished_at" {
                    *x = Value::Null;
                } else {
                    without_clock(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(without_clock),
        _ => {}
    }
}

#[tokio::test]
async fn serve_with_the_v2_flag_claims_wire2_captures_and_stores_a_wire2_report() -> Result<()> {
    let row = serve_one(true).await?;
    assert_completed(&row);
    // claim_with(Wire2) stored the mode; crawl_captured_with wrote the captures.
    assert_eq!(row.capture_mode.as_deref(), Some("wire2"));
    assert!(row.captures > 0, "no captures were written");
    let v: Value = serde_json::from_str(row.report.as_deref().context("no stored report")?)?;
    assert_eq!(v["schema_version"], 4);
    assert_eq!(v["policy_guidance"]["version"], "s14-wire-2");
    // Digest-form manifest rows (A9).
    for row in v["policy_guidance"]["manifest"]
        .as_array()
        .context("manifest rows")?
    {
        assert!(row.get("candidates_digest").is_some(), "{row}");
        assert!(row.get("candidates").is_none(), "{row}");
    }
    // Families were built from the captures, not degraded to capture_mode_changed.
    let families = v["evidence_families"]["families"]
        .as_object()
        .context("evidence_families")?;
    assert_eq!(families.len(), 6);
    let mut produced = 0;
    for (name, s) in families {
        assert_ne!(s["reason"], "capture_mode_changed", "{name}: {s}");
        assert_ne!(s["reason"], "analyzer_error", "{name}: {s}");
        if s["state"] != "unavailable" {
            produced += 1;
        }
    }
    assert!(produced >= 4, "only {produced} families produced");
    Ok(())
}

#[tokio::test]
async fn serve_without_the_v2_flag_claims_none_and_keeps_the_wire1_bytes() -> Result<()> {
    let row = serve_one(false).await?;
    assert_completed(&row);
    assert_eq!(row.capture_mode.as_deref(), Some("none"));
    assert_eq!(row.captures, 0, "wire-1 jobs take no captures");
    let stored = row.report.as_deref().context("no stored report")?;
    let mut served: Value = serde_json::from_str(stored)?;
    assert_eq!(served["policy_guidance"]["version"], "s14-wire-1");
    assert!(served.get("evidence_families").is_none());

    // The unchanged wire-1 pipeline, in-process, over the same fixtures.
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("s.sqlite").to_str().unwrap()).await?;
    let r = request(0x7700)?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::None).await?.unwrap();
    let mut unpaced = r.clone();
    unpaced.delay_ms = 0;
    let mut report =
        crawler::crawl_captured_with(&store, &unpaced, CaptureMode::None, &Mirror(fixtures()))
            .await?;
    report.request.delay_ms = r.delay_ms;
    let policy: AdvisoryPolicy =
        serde_json::from_slice(&std::fs::read("fixtures/policies/production-default.json")?)?;
    policy.validate()?;
    let expected = generate_wire_v1(&mut report, &policy)?;
    let mut expected: Value = serde_json::from_slice(&expected)?;

    without_clock(&mut served);
    without_clock(&mut expected);
    assert_eq!(
        serde_json::to_vec(&served)?,
        serde_json::to_vec(&expected)?,
        "the flag-off report must equal the wire-1 pipeline's bytes"
    );
    Ok(())
}
