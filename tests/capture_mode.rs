#![allow(clippy::format_collect)]
//! WP8b-2: the capture-mode carrier and worker-mode capture.
use seosmith::{crawler, families::types::CaptureMode, model::Report, store::Store};
use sqlx::Row;

#[path = "support/site.rs"]
mod site;
use site::{Site, request, unpaced};

async fn store() -> anyhow::Result<(tempfile::TempDir, Store)> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("s.sqlite").to_str().unwrap()).await?;
    Ok((dir, store))
}
async fn count(store: &Store, sql: &'static str, id: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(&store.0)
        .await
        .unwrap()
}
fn small_site() -> Site {
    Site::default()
        .robots("User-agent: *\nAllow: /\n")
        .html(
            "https://site.invalid/",
            "<html><head><title>Home</title></head><body><h1>Home</h1><a href='/a'>a</a><a href='/b'>b</a></body></html>",
        )
        .html("https://site.invalid/a", "<html><head><title>A</title></head><body><h1>A</h1></body></html>")
        .html("https://site.invalid/b", "<html><head><title>B</title></head><body><h1>B</h1></body></html>")
}
fn strip_times(report: &Report) -> serde_json::Value {
    let mut v = serde_json::to_value(report).unwrap();
    v["request"]["delay_ms"] = serde_json::Value::Null;
    v["started_at"] = serde_json::Value::Null;
    v["finished_at"] = serde_json::Value::Null;
    v
}

#[tokio::test]
async fn claim_with_wire2_stores_wire2_and_claim_stores_none() -> anyhow::Result<()> {
    let (_d, store) = store().await?;
    let a = request(5, 0);
    let b = request(5, 0);
    store.submit(&a).await?;
    store.submit(&b).await?;
    let first = store.claim_with(CaptureMode::Wire2).await?.unwrap();
    let second = store.claim().await?.unwrap();
    assert_eq!(
        store.capture_mode(&first.request_id.to_string()).await?,
        CaptureMode::Wire2
    );
    assert_eq!(
        store.capture_mode(&second.request_id.to_string()).await?,
        CaptureMode::None
    );
    Ok(())
}

#[tokio::test]
async fn recovered_job_keeps_its_first_claim_mode_in_both_directions() -> anyhow::Result<()> {
    let (_d, store) = store().await?;
    let wire2 = request(5, 0);
    let none = request(5, 0);
    store.submit(&wire2).await?;
    sqlx::query("UPDATE jobs SET created=created-100 WHERE id=?")
        .bind(wire2.request_id.to_string())
        .execute(&store.0)
        .await?;
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    store.submit(&none).await?;
    store.claim_with(CaptureMode::None).await?.unwrap();
    store.recover().await?;
    // Both are queued again (the wire2 job is older, so it is claimed first); claim
    // them with the opposite mode.
    store.claim_with(CaptureMode::None).await?.unwrap();
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    assert_eq!(
        store.capture_mode(&wire2.request_id.to_string()).await?,
        CaptureMode::Wire2
    );
    assert_eq!(
        store.capture_mode(&none.request_id.to_string()).await?,
        CaptureMode::None
    );
    Ok(())
}

#[tokio::test]
async fn a_job_from_before_the_column_reads_as_none() -> anyhow::Result<()> {
    let (_d, store) = store().await?;
    let r = request(5, 0);
    store.submit(&r).await?;
    store.claim().await?.unwrap();
    sqlx::query("UPDATE jobs SET capture_mode=NULL")
        .execute(&store.0)
        .await?;
    assert_eq!(
        store.capture_mode(&r.request_id.to_string()).await?,
        CaptureMode::None
    );
    Ok(())
}

#[tokio::test]
async fn crawl_captured_none_is_byte_identical_to_crawl() -> anyhow::Result<()> {
    let r = request(5, 0);
    let (_d1, plain) = store().await?;
    plain.submit(&r).await?;
    plain.claim().await?.unwrap();
    let expected = crawler::crawl_with(&plain, &unpaced(&r), &small_site()).await?;
    let (_d2, captured) = store().await?;
    captured.submit(&r).await?;
    captured.claim_with(CaptureMode::None).await?.unwrap();
    let actual =
        crawler::crawl_captured_with(&captured, &unpaced(&r), CaptureMode::None, &small_site())
            .await?;
    assert_eq!(strip_times(&actual), strip_times(&expected));
    let id = r.request_id.to_string();
    assert_eq!(
        count(
            &captured,
            "SELECT count(*) FROM local_page_evidence WHERE job_id=?",
            &id
        )
        .await,
        0
    );
    Ok(())
}

#[tokio::test]
async fn wire2_job_writes_evidence_and_the_same_report() -> anyhow::Result<()> {
    let r = request(5, 0);
    let (_d1, plain) = store().await?;
    plain.submit(&r).await?;
    plain.claim().await?.unwrap();
    let expected = crawler::crawl_with(&plain, &unpaced(&r), &small_site()).await?;
    let (_d2, store) = store().await?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    let actual =
        crawler::crawl_captured_with(&store, &unpaced(&r), CaptureMode::Wire2, &small_site())
            .await?;
    // Capturing never changes the report the worker delivers.
    assert_eq!(strip_times(&actual), strip_times(&expected));
    let id = r.request_id.to_string();
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM local_page_evidence WHERE job_id=?",
            &id
        )
        .await,
        i64::try_from(actual.pages.len())?
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM local_robots_run WHERE job_id=?",
            &id
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM local_sitemap_run WHERE job_id=?",
            &id
        )
        .await,
        1
    );
    let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id=?")
        .bind(&id)
        .fetch_one(&store.0)
        .await?;
    assert_eq!(state, "running");
    Ok(())
}

#[tokio::test]
async fn capture_refuses_a_standard_job_not_claimed_as_wire2() -> anyhow::Result<()> {
    let r = request(5, 0);
    let (_d, store) = store().await?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::None).await?.unwrap();
    // A crawl asked to capture for a job that was not claimed as wire2 must fail
    // before it records anything.
    assert!(
        crawler::crawl_captured_with(&store, &unpaced(&r), CaptureMode::Wire2, &small_site())
            .await
            .is_err()
    );
    let id = r.request_id.to_string();
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM local_page_evidence WHERE job_id=?",
            &id
        )
        .await,
        0
    );
    Ok(())
}

#[tokio::test]
async fn evidence_beyond_sixteen_mib_becomes_a_budget_omission() -> anyhow::Result<()> {
    let r = request(5, 0);
    let (_d, store) = store().await?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    let id = r.request_id.to_string();
    sqlx::query("INSERT INTO local_evidence_usage(job_id,evidence_bytes) VALUES(?,?)")
        .bind(&id)
        .bind(16_i64 * 1024 * 1024)
        .execute(&store.0)
        .await?;
    crawler::crawl_captured_with(&store, &unpaced(&r), CaptureMode::Wire2, &small_site()).await?;
    let rows = sqlx::query("SELECT evidence FROM local_page_evidence WHERE job_id=?")
        .bind(&id)
        .fetch_all(&store.0)
        .await?;
    assert!(!rows.is_empty());
    for row in rows {
        let v: serde_json::Value = serde_json::from_str(&row.get::<String, _>("evidence"))?;
        let collectors = v["capture"]["collectors"].as_array().unwrap();
        assert_eq!(collectors.len(), 8);
        assert!(collectors.iter().all(|c| c["reason"] == "run_byte_limit"));
    }
    Ok(())
}

fn many_links_site(links: usize) -> Site {
    let mut body = String::new();
    for n in 0..links {
        use std::fmt::Write;
        write!(
            body,
            "<a href='/long/path/segment/{n}/with/some/more/text'>link number {n} text</a>"
        )
        .unwrap();
    }
    Site::default().robots("User-agent: *\nAllow: /\n").html(
        "https://site.invalid/",
        &format!("<html><head><title>H</title></head><body><h1>H</h1>{body}</body></html>"),
    )
}
async fn graph_state(store: &Store, id: &str) -> (String, Option<String>) {
    let raw: String = sqlx::query_scalar(
        "SELECT evidence FROM local_page_evidence WHERE job_id=? AND url='https://site.invalid/'",
    )
    .bind(id)
    .fetch_one(&store.0)
    .await
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let g = v["capture"]["collectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["collector"] == "graph")
        .unwrap()
        .clone();
    (
        g["state"].as_str().unwrap().into(),
        g["reason"].as_str().map(str::to_owned),
    )
}

#[tokio::test]
async fn wire2_graph_limit_is_64_kib_and_local_mode_stays_24_kib() -> anyhow::Result<()> {
    // About 180 links: over 24 KiB of graph JSON, under 64 KiB.
    let mut r = request(1, 0);
    r.max_pages = 1;
    let (_d, store) = store().await?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    crawler::crawl_captured_with(
        &store,
        &unpaced(&r),
        CaptureMode::Wire2,
        &many_links_site(180),
    )
    .await?;
    let (state, reason) = graph_state(&store, &r.request_id.to_string()).await;
    assert_eq!(
        (state.as_str(), reason),
        ("observed", None),
        "wire2 keeps 180 links"
    );

    let local = request(1, 0);
    let (_d2, lstore) = store_local().await?;
    lstore.submit_local(&local).await?;
    let id = local.request_id.to_string();
    lstore.claim_local(&id).await?.unwrap();
    crawler::crawl_local_with(&lstore, &local, &many_links_site(180)).await?;
    let (state, reason) = graph_state(&lstore, &id).await;
    assert_eq!(state, "unavailable");
    assert_eq!(
        reason.as_deref(),
        Some("collector_byte_limit"),
        "local mode stays at 24 KiB"
    );
    Ok(())
}
async fn store_local() -> anyhow::Result<(tempfile::TempDir, Store)> {
    store().await
}

#[tokio::test]
async fn prune_removes_evidence_with_the_frontier() -> anyhow::Result<()> {
    let r = request(5, 0);
    let (_d, store) = store().await?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    crawler::crawl_captured_with(&store, &unpaced(&r), CaptureMode::Wire2, &small_site()).await?;
    let id = r.request_id.to_string();
    sqlx::query(
        "UPDATE jobs SET delivery_state='delivered', created=unixepoch()-3000000 WHERE id=?",
    )
    .bind(&id)
    .execute(&store.0)
    .await?;
    store.prune().await?;
    for sql in [
        "SELECT count(*) FROM local_page_evidence WHERE job_id=?",
        "SELECT count(*) FROM local_robots_run WHERE job_id=?",
        "SELECT count(*) FROM local_sitemap_run WHERE job_id=?",
        "SELECT count(*) FROM local_evidence_usage WHERE job_id=?",
        "SELECT count(*) FROM frontier WHERE job_id=?",
    ] {
        assert_eq!(count(&store, sql, &id).await, 0, "{sql}");
    }
    Ok(())
}

fn sitemap_site(entries: &[&str]) -> Site {
    let urls: String = entries
        .iter()
        .map(|u| format!("<url><loc>https://site.invalid{u}</loc></url>"))
        .collect();
    small_site()
        .robots("User-agent: *\nAllow: /\nSitemap: https://site.invalid/sitemap.xml\n")
        .raw(
            "https://site.invalid/sitemap.xml",
            200,
            &[("content-type", "application/xml")],
            format!(
                "<?xml version=\"1.0\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{urls}</urlset>"
            )
            .as_bytes(),
        )
}

/// A worker restart resumes a wire2 job with a fresh discovery run. A changed
/// sitemap must not fail the job (a plain job survives the same restart).
#[tokio::test]
async fn resumed_wire2_job_survives_a_changed_sitemap() -> anyhow::Result<()> {
    let r = request(5, 0);
    let (_d, store) = store().await?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    crawler::crawl_captured_with(
        &store,
        &unpaced(&r),
        CaptureMode::Wire2,
        &sitemap_site(&["/", "/a", "/b"]),
    )
    .await?;
    store.recover().await?;
    store.claim_with(CaptureMode::None).await?.unwrap();
    let id = r.request_id.to_string();
    assert_eq!(store.capture_mode(&id).await?, CaptureMode::Wire2);
    let stored = store.capture_mode(&id).await?;
    let resumed =
        crawler::crawl_captured_with(&store, &unpaced(&r), stored, &sitemap_site(&["/", "/a"]))
            .await;
    assert!(resumed.is_ok(), "resume failed: {resumed:?}");
    // The stored sitemap evidence now describes the run that finished last.
    let evidence: String =
        sqlx::query_scalar("SELECT evidence FROM local_sitemap_run WHERE job_id=?")
            .bind(&id)
            .fetch_one(&store.0)
            .await?;
    assert!(!evidence.contains("/b"), "stale sitemap evidence kept");
    // And robots evidence lists the origin once, however often the job resumed.
    let robots: String = sqlx::query_scalar("SELECT evidence FROM local_robots_run WHERE job_id=?")
        .bind(&id)
        .fetch_one(&store.0)
        .await?;
    let robots: serde_json::Value = serde_json::from_str(&robots)?;
    assert_eq!(robots["origins"].as_array().unwrap().len(), 1);
    Ok(())
}

/// Cancelling a wire2 job mid-crawl is a `cancelled` outcome, not a crawl error.
#[tokio::test]
async fn cancelled_wire2_job_reports_cancelled() -> anyhow::Result<()> {
    let r = request(5, 0);
    let (_d, store) = store().await?;
    store.submit(&r).await?;
    store.claim_with(CaptureMode::Wire2).await?.unwrap();
    let id = r.request_id.to_string();
    sqlx::query("UPDATE jobs SET state='cancelled' WHERE id=?")
        .bind(&id)
        .execute(&store.0)
        .await?;
    let report =
        crawler::crawl_captured_with(&store, &unpaced(&r), CaptureMode::Wire2, &small_site()).await;
    assert!(report.is_ok(), "cancel surfaced as error: {report:?}");
    Ok(())
}
