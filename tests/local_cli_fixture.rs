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
    request.max_pages = 9;
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
            serde_json::json!({"status":200,"body":"User-agent: *\r\nDisallow: /private\rUser-agent: Googlebot\nAllow: /private\n"}),
        ),
        (
            "https://fixture.invalid/sitemap.xml",
            serde_json::json!({"status":404,"body":""}),
        ),
        (
            "https://fixture.invalid/",
            serde_json::json!({"status":200,"header_fields":[["content-type","text/html"],["x-robots-tag","Googlebot: noindex"],["x-robots-tag","GPTBot: noindex"]],"body":format!("<html><head><meta name='robots' content='index'><link rel='canonical' href='/'></head><body><a href='/next'>{}</a><a href='/private'>Private</a><a href='/candidate'>Candidate</a><a href='/loading'>Loading</a><a href='/plain'>Plain</a><a href='/decode'>Decode</a><a href='/large'>Large</a></body></html>", "n".repeat(300))}),
        ),
        (
            "https://fixture.invalid/next",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html"},"body":"<html><body><main><h1>Fixture</h1>Useful short page.</main></body></html>"}),
        ),
        (
            "https://fixture.invalid/candidate",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html"},"body":"<html><head><title>Page not found</title></head><body><main><h1>Page not found</h1>Sorry, that page is missing.</main></body></html>"}),
        ),
        (
            "https://fixture.invalid/loading",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html"},"body":"<html><body><main>Loading...</main></body></html>"}),
        ),
        (
            "https://fixture.invalid/plain",
            serde_json::json!({"status":200,"headers":{"content-type":"text/plain"},"body":"Plain text"}),
        ),
        (
            "https://fixture.invalid/decode",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html; charset=utf-16le"},"body":"x"}),
        ),
        (
            "https://fixture.invalid/large",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html"},"body":"x".repeat(256 * 1024 + 1)}),
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
    assert_eq!(result["pages"].as_array().unwrap().len(), 8);
    let store = Store::open(db.to_str().unwrap()).await?;
    let captures: Vec<(String, String)> =
        sqlx::query_as("SELECT url,evidence FROM local_page_evidence WHERE job_id=? ORDER BY url")
            .bind(&id)
            .fetch_all(&store.0)
            .await?;
    assert_eq!(captures.len(), 8);
    for (url, raw) in captures {
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(value["capture"]["url"], url);
        let detector = &value["capture"]["collectors"][6];
        assert_eq!(detector["collector"], "soft_404");
        if url.ends_with("/private") {
            assert_eq!(detector["state"], "unavailable");
            assert_eq!(detector["reason"], "blocked_by_robots");
        } else {
            assert_eq!(detector["data"]["semantics"], "s06-local-detector-2");
            if let Some(reason) = [
                ("/plain", "not HTML"),
                ("/decode", "decoding errors"),
                ("/large", "source exceeds byte limit"),
            ]
            .iter()
            .find_map(|(suffix, reason)| url.ends_with(suffix).then_some(reason))
            {
                assert_eq!(detector["state"], "unavailable");
                assert_eq!(detector["reason"], *reason);
                assert_eq!(detector["data"]["decision"], "unknown");
                assert_eq!(detector["data"]["reason"], *reason);
                assert!(detector["data"]["evidence"].is_null());
            } else if url.ends_with("/candidate") {
                assert_eq!(detector["state"], "observed");
                assert_eq!(detector["data"]["decision"], "candidate");
                assert_eq!(detector["data"]["evidence"]["h1"], "Page not found");
            } else if url.ends_with("/next") {
                assert_eq!(detector["state"], "observed");
                assert_eq!(detector["data"]["decision"], "no_candidate");
            } else if url.ends_with("/loading") {
                assert_eq!(detector["state"], "partial");
                assert_eq!(
                    detector["reason"],
                    "main content or rendered outcome unavailable"
                );
                assert!(detector["data"]["evidence"].is_object());
                assert_eq!(detector["data"]["decision"], "unknown");
                assert_eq!(
                    detector["data"]["reason"],
                    "main content or rendered outcome unavailable"
                );
            }
        }
        let robots = &value["capture"]["collectors"][3]["data"];
        assert_eq!(robots["source"], "robots_txt");
        if url.ends_with("/private") {
            assert_eq!(robots["decisions"][0]["allowed"], false);
            assert_eq!(robots["decisions"][0]["matched_line"], 2);
            assert_eq!(robots["decisions"][0]["raw_line"], "Disallow: /private");
            assert_eq!(robots["decisions"][1]["allowed"], true);
            let indexing = &value["capture"]["collectors"][5]["data"];
            assert_eq!(indexing["state"], "robots_blocked");
            assert_eq!(indexing["decisions"][1]["directives"]["index"], "Unknown");
            assert_eq!(indexing["decisions"][1]["intent"], "Unknown");
            assert_eq!(
                value["capture"]["collectors"][0]["reason"],
                "blocked_by_robots"
            );
        } else if url.ends_with('/') {
            let fields = &value["capture"]["collectors"][4]["data"]["occurrences"];
            assert_eq!(fields.as_array().unwrap().len(), 2);
            assert_eq!(fields[1]["ordinal"], 1);
            let indexing = &value["capture"]["collectors"][5]["data"];
            assert_eq!(
                indexing["decisions"][1]["directives"]["index"],
                "Restricted"
            );
            assert_eq!(indexing["decisions"][1]["intent"], "Unknown");
            assert_eq!(
                indexing["decisions"][3]["directives"]["index"],
                "Restricted"
            );
            assert_eq!(indexing["decisions"][2]["directives"]["index"], "Allowed");
            assert_eq!(
                indexing["decisions"][1]["directives"]["conflicts"][0],
                "index/noindex"
            );
            assert_eq!(
                indexing["decisions"][1]["directives"]["applicable"][0]["locator"]["ordinal"],
                0
            );
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

#[tokio::test]
async fn policy_cli_short_missing_and_unavailable_robots_survive_reopen() -> anyhow::Result<()> {
    for (status, body, source, page_count) in [
        (200, "", "robots_txt", 1),
        (404, "", "not_found", 1),
        (503, "", "unavailable", 0),
        (403, "", "http_denied_synthetic", 1),
    ] {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("db.sqlite");
        let mut request: seosmith::model::CrawlRequest = serde_json::from_value(
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../fixtures/contracts/legacy.json"
            ))?["request"]
                .clone(),
        )?;
        request.request_id = Uuid::new_v4();
        request.url = "https://fixture.invalid/".parse()?;
        request.max_pages = 1;
        request.max_resources = 0;
        request.max_duration_seconds = 30;
        request.delay_ms = 1000;
        request.check_external_links = false;
        let request_path = dir.path().join("request.json");
        let policy_path = dir.path().join("policy.json");
        let fixture_path = dir.path().join("responses.json");
        std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
        std::fs::write(&policy_path, "{}")?;
        std::fs::write(
            &fixture_path,
            serde_json::to_vec(&serde_json::json!({
                "https://fixture.invalid/robots.txt": {"status":status,"body":body},
                "https://fixture.invalid/sitemap.xml": {"status":404,"body":""},
                "https://fixture.invalid/": {"status":200,"headers":{"content-type":"text/html"},"body":"<html></html>"}
            }))?,
        )?;
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
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let store = Store::open(db.to_str().unwrap()).await?;
        let id = request.request_id.to_string();
        let run: String =
            sqlx::query_scalar("SELECT evidence FROM local_robots_run WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        let run: serde_json::Value = serde_json::from_str(&run)?;
        assert_eq!(run["origins"][0]["origin"], "https://fixture.invalid");
        assert_eq!(run["origins"][0]["source"], source);
        let records: Vec<String> =
            sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=?")
                .bind(&id)
                .fetch_all(&store.0)
                .await?;
        assert_eq!(records.len(), page_count);
        if let Some(record) = records.first() {
            let value: serde_json::Value = serde_json::from_str(record)?;
            let robots = &value["capture"]["collectors"][3]["data"];
            assert_eq!(robots["source"], source);
            assert_eq!(robots["decisions"][0]["allowed"], status != 403);
        }
    }
    Ok(())
}

#[tokio::test]
async fn policy_cli_keeps_different_origin_policies_separate() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("db.sqlite");
    let report: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let mut request = report.request;
    request.request_id = Uuid::new_v4();
    request.url = "https://fixture.invalid/".parse()?;
    request.max_pages = 1;
    request.max_resources = 1;
    request.max_duration_seconds = 30;
    request.delay_ms = 1000;
    request.check_external_links = true;
    let request_path = dir.path().join("request.json");
    let policy_path = dir.path().join("policy.json");
    let fixture_path = dir.path().join("responses.json");
    std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
    std::fs::write(&policy_path, "{}")?;
    std::fs::write(
        &fixture_path,
        serde_json::to_vec(&serde_json::json!({
            "https://fixture.invalid/robots.txt": {"status":404,"body":""},
            "https://fixture.invalid/sitemap.xml": {"status":404,"body":""},
            "https://fixture.invalid/": {"status":200,"headers":{"content-type":"text/html"},"body":"<html><body><a href='https://other.invalid/private'>external</a></body></html>"},
            "https://other.invalid/robots.txt": {"status":200,"body":"User-agent: *\nDisallow: /private\n"}
        }))?,
    )?;
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = Store::open(db.to_str().unwrap()).await?;
    let id = request.request_id.to_string();
    let run: String = sqlx::query_scalar("SELECT evidence FROM local_robots_run WHERE job_id=?")
        .bind(&id)
        .fetch_one(&store.0)
        .await?;
    let run: serde_json::Value = serde_json::from_str(&run)?;
    assert_eq!(run["origins"].as_array().unwrap().len(), 2);
    assert_eq!(run["origins"][1]["origin"], "https://other.invalid");
    let raw: String =
        sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=? AND url=?")
            .bind(&id)
            .bind("https://other.invalid/private")
            .fetch_one(&store.0)
            .await?;
    let capture: serde_json::Value = serde_json::from_str(&raw)?;
    assert_eq!(
        capture["capture"]["collectors"][3]["data"]["decisions"][0]["allowed"],
        false
    );
    Ok(())
}

#[tokio::test]
async fn saved_graph_reads_real_crawl_without_mutating_store() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("graph.sqlite");
    let report: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let mut request = report.request;
    request.request_id = Uuid::new_v4();
    request.url = "https://fixture.invalid/".parse()?;
    request.max_pages = 8;
    request.max_resources = 0;
    request.max_duration_seconds = 30;
    request.delay_ms = 1000;
    request.check_external_links = false;
    let request_path = dir.path().join("request.json");
    let policy_path = dir.path().join("policy.json");
    let fixture_path = dir.path().join("responses.json");
    std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
    std::fs::write(&policy_path, "{}")?;
    std::fs::write(
        &fixture_path,
        serde_json::to_vec(&serde_json::json!({
            "https://fixture.invalid/robots.txt": {"status":200,"body":"User-agent: *\nDisallow: /private\n"},
            "https://fixture.invalid/sitemap.xml": {"status":200,"body":"<?xml version='1.0'?><urlset xmlns='http://www.sitemaps.org/schemas/sitemap/0.9'><url><loc>https://fixture.invalid/hidden</loc></url></urlset>"},
            "https://fixture.invalid/": {"status":200,"headers":{"content-type":"text/html"},"body":"<a href='/next#missing'>next</a><a href='/private'>private</a><a href='mailto:test@example.com'>rejected</a>"},
            "https://fixture.invalid/next": {"status":200,"headers":{"content-type":"text/html"},"body":"<h1 id='here'>Next</h1>"},
            "https://fixture.invalid/hidden": {"status":200,"headers":{"content-type":"text/html"},"body":"<p>Hidden</p>"}
        }))?,
    )?;
    let crawl = Command::new(env!("CARGO_BIN_EXE_seosmith"))
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
        crawl.status.success(),
        "{}",
        String::from_utf8_lossy(&crawl.stderr)
    );
    // Explicitly quiesce/checkpoint the completed fixture before immutable analysis.
    let quiesce = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&db)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
    )
    .await?;
    quiesce.close().await;
    let before = std::fs::read(&db)?;
    let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            db.to_str().unwrap(),
            "saved-graph",
            &request.request_id.to_string(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, std::fs::read(&db)?);
    assert!(!db.with_extension("sqlite-wal").exists());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["coverage"]["complete"], false);
    let nodes = result["nodes"].as_array().unwrap();
    assert_eq!(
        nodes
            .iter()
            .find(|n| n["url"] == "https://fixture.invalid/hidden")
            .unwrap()["reachability"],
        "unknown"
    );
    assert_eq!(
        nodes
            .iter()
            .find(|n| n["url"] == "https://fixture.invalid/next")
            .unwrap()["navigation_distance"],
        1
    );
    assert!(
        result["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["collector_reason"] == "blocked_by_robots")
    );
    assert!(
        result["coverage"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "extraction_truncated")
    );
    assert_eq!(result["fragments"][0]["state"], "absent");
    let journal = dir.path().join("graph.sqlite-journal");
    std::fs::write(&journal, b"hot")?;
    let rejected = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            db.to_str().unwrap(),
            "saved-graph",
            &request.request_id.to_string(),
        ])
        .output()?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("rollback journal"));
    std::fs::remove_file(journal)?;
    let missing = dir.path().join("missing.sqlite");
    let rejected = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            missing.to_str().unwrap(),
            "saved-graph",
            &request.request_id.to_string(),
        ])
        .output()?;
    assert!(!rejected.status.success());
    assert!(!missing.exists());
    // Corrupt a copy to verify duplicate collectors are reported, not selected.
    let copy = dir.path().join("copy.sqlite");
    std::fs::copy(&db, &copy)?;
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&copy)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
    )
    .await?;
    let raw: String =
        sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=? AND url=?")
            .bind(request.request_id.to_string())
            .bind("https://fixture.invalid/next")
            .fetch_one(&pool)
            .await?;
    let mut evidence: serde_json::Value = serde_json::from_str(&raw)?;
    let collectors = evidence["capture"]["collectors"].as_array_mut().unwrap();
    let duplicate = collectors
        .iter()
        .find(|v| v["collector"] == "graph")
        .unwrap()
        .clone();
    collectors.push(duplicate);
    sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
        .bind(serde_json::to_string(&evidence)?)
        .bind(request.request_id.to_string())
        .bind("https://fixture.invalid/next")
        .execute(&pool)
        .await?;
    for (path, digest_source) in [("hidden", "hidden"), ("private", "other")] {
        let url = format!("https://fixture.invalid/{path}");
        let digest_url = format!("https://fixture.invalid/{digest_source}");
        let marker = serde_json::json!({"capture":{
            "version":1, "url":null, "url_omission":"run_byte_limit",
            "url_sha256":seosmith::model::digest(digest_url.as_bytes())
        }});
        let changed =
            sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
                .bind(marker.to_string())
                .bind(request.request_id.to_string())
                .bind(url)
                .execute(&pool)
                .await?;
        assert_eq!(changed.rows_affected(), 1);
    }
    pool.close().await;
    let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            copy.to_str().unwrap(),
            "saved-graph",
            &request.request_id.to_string(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        result["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["reason"] == "duplicate_graph_collector")
    );
    let omitted = result["omitted"].as_array().unwrap();
    for (path, reason) in [
        ("hidden", "run_byte_limit"),
        ("private", "unsupported_or_unbound_capture"),
    ] {
        let url = format!("https://fixture.invalid/{path}");
        assert!(
            omitted
                .iter()
                .any(|o| o["url"] == url && o["reason"] == reason)
        );
    }
    // Exercise the aggregate budget with individually valid, bounded envelopes.
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&copy)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
    )
    .await?;
    for index in 0..40 {
        let url = format!("https://fixture.invalid/budget-{index:02}");
        let envelope = serde_json::json!({"capture":{"version":1,"url":url,"padding":"x".repeat(60_000),"collectors":[{"collector":"graph","version":1,"state":"observed","data":{"url":url,"links":[],"ids":[],"extraction_complete":true}}]}});
        sqlx::query("INSERT INTO frontier(job_id,url,depth,in_sitemap,page) VALUES(?,?,1,0,'{}')")
            .bind(request.request_id.to_string())
            .bind(&url)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO local_page_evidence(job_id,url,evidence) VALUES(?,?,?)")
            .bind(request.request_id.to_string())
            .bind(&url)
            .bind(serde_json::to_string(&envelope)?)
            .execute(&pool)
            .await?;
    }
    let long_url = format!("https://fixture.invalid/z{}", "x".repeat(9000));
    sqlx::query("INSERT INTO frontier(job_id,url,depth,in_sitemap,page) VALUES(?,?,1,0,'{}')")
        .bind(request.request_id.to_string())
        .bind(&long_url)
        .execute(&pool)
        .await?;
    pool.close().await;
    let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            copy.to_str().unwrap(),
            "saved-graph",
            &request.request_id.to_string(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let budget: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        budget["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["reason"] == "aggregate_input_limit")
    );
    assert!(
        budget["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["reason"] == "url_byte_limit" && o["url"].is_null())
    );
    assert_eq!(budget["coverage"]["complete"], false);
    assert_eq!(
        budget["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["url"] == "https://fixture.invalid/budget-00")
            .unwrap()["reachability"],
        "unknown"
    );

    // A pre-mode schema is rejected without creating or migrating anything.
    let legacy = dir.path().join("pre-mode.sqlite");
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&legacy)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
    )
    .await?;
    sqlx::query("CREATE TABLE jobs (id TEXT PRIMARY KEY, request TEXT, state TEXT)")
        .execute(&pool)
        .await?;
    pool.close().await;
    let legacy_before = std::fs::read(&legacy)?;
    let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            legacy.to_str().unwrap(),
            "saved-graph",
            &request.request_id.to_string(),
        ])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("legacy jobs schema"));
    assert_eq!(legacy_before, std::fs::read(&legacy)?);
    Ok(())
}
