#![cfg(feature = "local-cli-fixtures")]

use seosmith::{model::Report, store::Store};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::{collections::BTreeMap, process::Command};
use uuid::Uuid;

#[tokio::test]
async fn policy_cli_fixtures_upgrade_and_reopen_without_delivery_changes() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("legacy.sqlite");
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true),
    )
    .await?;
    sqlx::raw_sql("CREATE TABLE jobs (id TEXT PRIMARY KEY, request TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued', started_at TEXT, report TEXT, report_hash TEXT, error TEXT, delivery_state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0, next_attempt INTEGER NOT NULL DEFAULT 0, discovery_truncated INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL DEFAULT (unixepoch())); CREATE TABLE frontier (job_id TEXT NOT NULL REFERENCES jobs(id), url TEXT NOT NULL, depth INTEGER NOT NULL, in_sitemap INTEGER NOT NULL, page TEXT, PRIMARY KEY(job_id,url));").execute(&pool).await?;
    let old_report = include_str!("../fixtures/contracts/legacy.json");
    let report: Report = serde_json::from_str(old_report)?;
    let pending_id = report.request.request_id.to_string();
    let old_hash = seosmith::model::digest(old_report.as_bytes());
    let old_request = serde_json::to_string(&report.request)?;
    let mut delivered_report: serde_json::Value = serde_json::from_str(old_report)?;
    let delivered_id = Uuid::new_v4().to_string();
    delivered_report["request"]["request_id"] = delivered_id.clone().into();
    let delivered_request = serde_json::to_string(&delivered_report["request"])?;
    let delivered_bytes = serde_json::to_string(&delivered_report)?;
    let delivered_hash = seosmith::model::digest(delivered_bytes.as_bytes());
    for (old_id, request, body, hash, delivery, attempts) in [
        (
            pending_id.as_str(),
            old_request.as_str(),
            old_report,
            old_hash.as_str(),
            "pending",
            3,
        ),
        (
            delivered_id.as_str(),
            delivered_request.as_str(),
            delivered_bytes.as_str(),
            delivered_hash.as_str(),
            "delivered",
            4,
        ),
    ] {
        sqlx::query("INSERT INTO jobs(id,request,state,report,report_hash,delivery_state,attempts) VALUES(?,?,'completed',?,?,?,?)").bind(old_id).bind(request).bind(body).bind(hash).bind(delivery).bind(attempts).execute(&pool).await?;
    }
    pool.close().await;
    let mut request = report.request;
    request.request_id = Uuid::new_v4();
    request.url = "https://fixture.invalid/".parse()?;
    request.max_pages = 4;
    request.max_resources = 0;
    request.max_duration_seconds = 30;
    request.delay_ms = 1000;
    request.check_external_links = false;
    let id = request.request_id.to_string();
    let request_path = dir.path().join("request.json");
    let policy_path = dir.path().join("policy.json");
    let fixture_path = dir.path().join("responses.json");
    std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
    std::fs::write(&policy_path, "{}")?;
    let fixtures = BTreeMap::from([
        (
            "https://fixture.invalid/robots.txt",
            serde_json::json!({"status":200,"body":"User-agent: *\nDisallow: /private\n"}),
        ),
        (
            "https://fixture.invalid/sitemap.xml",
            serde_json::json!({"status":404,"body":""}),
        ),
        (
            "https://fixture.invalid/",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html"},"body":format!("<html><head><meta name='robots' content='index'><link rel='canonical' href='/'></head><body><a href='/next'>{}</a><a href='/private'>Private</a></body></html>", "n".repeat(300))}),
        ),
        (
            "https://fixture.invalid/next",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html"},"body":"<html><body><h1>Fixture</h1></body></html>"}),
        ),
    ]);
    std::fs::write(&fixture_path, serde_json::to_vec(&fixtures)?)?;
    let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .env("SEOSMITH_LOCAL_FIXTURES", &fixture_path)
        .args([
            "--database",
            db.to_str().unwrap(),
            "--advisory-policy",
            policy_path.to_str().unwrap(),
            "crawl",
            "--request",
            request_path.to_str().unwrap(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["request"]["request_id"], id);
    assert_eq!(result["pages"].as_array().unwrap().len(), 3);
    let store = Store::open(db.to_str().unwrap()).await?;
    let captures: Vec<(String, String)> =
        sqlx::query_as("SELECT url,evidence FROM local_page_evidence WHERE job_id=? ORDER BY url")
            .bind(&id)
            .fetch_all(&store.0)
            .await?;
    assert_eq!(captures.len(), 3);
    for (url, raw) in captures {
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(value["capture"]["url"], url);
        if url.ends_with("/private") {
            assert_eq!(
                value["capture"]["collectors"][0]["reason"],
                "blocked_by_robots"
            );
        } else if url.ends_with('/') {
            assert_eq!(value["capture"]["collectors"][0]["state"], "observed");
            assert_eq!(
                value["capture"]["collectors"][0]["data"]["occurrences"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(value["capture"]["collectors"][1]["data"]["url"], url);
            assert_eq!(value["capture"]["collectors"][1]["state"], "partial");
            assert_eq!(
                value["capture"]["collectors"][1]["reason"],
                "source_or_extraction_incomplete"
            );
            assert_eq!(
                value["capture"]["collectors"][1]["data"]["extraction_complete"],
                false
            );
        }
    }
    assert_eq!(store.view(&id).await?.unwrap().state, "local_only");
    assert!(store.report(&id).await?.is_none());
    for (old_id, body, expected_hash, delivery, attempts) in [
        (
            pending_id.as_str(),
            old_report,
            old_hash.as_str(),
            "pending",
            3,
        ),
        (
            delivered_id.as_str(),
            delivered_bytes.as_str(),
            delivered_hash.as_str(),
            "delivered",
            4,
        ),
    ] {
        assert_eq!(store.report(old_id).await?.as_deref(), Some(body));
        let hash: String = sqlx::query_scalar("SELECT report_hash FROM jobs WHERE id=?")
            .bind(old_id)
            .fetch_one(&store.0)
            .await?;
        assert_eq!(hash, expected_hash);
        let view = store.view(old_id).await?.unwrap();
        assert_eq!(
            (view.delivery_state.as_str(), view.attempts),
            (delivery, attempts)
        );
    }
    let pending = store.pending().await?.unwrap();
    assert_eq!(
        (
            pending.0.as_str(),
            pending.1.as_str(),
            pending.2.as_str(),
            pending.3
        ),
        (pending_id.as_str(), old_report, old_hash.as_str(), 3)
    );
    assert!(
        store
            .delivered(&pending_id, "incorrect-hash")
            .await
            .is_err()
    );
    store.delivered(&pending_id, &old_hash).await?;
    assert!(store.pending().await?.is_none());
    assert_eq!(
        store.report(&pending_id).await?.as_deref(),
        Some(old_report)
    );
    assert_eq!(store.view(&delivered_id).await?.unwrap().attempts, 4);
    Ok(())
}
