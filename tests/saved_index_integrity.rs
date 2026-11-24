#![cfg(feature = "local-cli-fixtures")]

use seosmith::{model::Report, store::Store};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::process::Command;
use uuid::Uuid;

#[tokio::test]
async fn saved_index_integrity_reads_real_crawl_without_mutation() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("saved-index.sqlite");
    let report: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let mut request = report.request;
    request.request_id = Uuid::new_v4();
    request.url = "https://fixture.invalid/".parse()?;
    request.max_pages = 12;
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
            "https://fixture.invalid/robots.txt": {
                "status": 200,
                "body": "User-agent: *\nDisallow: /private\nUser-agent: GPTBot\nDisallow: /training\nSitemap: https://fixture.invalid/sitemap.xml\n"
            },
            "https://fixture.invalid/sitemap.xml": {
                "status": 200,
                "body": "<sitemapindex><sitemap><loc>https://fixture.invalid/child.xml</loc></sitemap><sitemap><loc>https://fixture.invalid/child.xml</loc></sitemap><sitemap><loc>https://fixture.invalid/blocked.xml</loc></sitemap><sitemap><loc>https://fixture.invalid/error.xml</loc></sitemap></sitemapindex>"
            },
            "https://fixture.invalid/child.xml": {
                "status": 200,
                "body": "<sitemapindex><sitemap><loc>https://fixture.invalid/leaf.xml</loc></sitemap></sitemapindex>"
            },
            "https://fixture.invalid/leaf.xml": {
                "status": 200,
                "body": "<urlset><url><loc>https://fixture.invalid/page</loc><lastmod>not-a-date</lastmod></url><url><loc>https://fixture.invalid/hidden</loc><lastmod>2026-11-01</lastmod></url><url><loc>bad</loc></url><url></url></urlset>"
            },
            "https://fixture.invalid/error.xml": {"status": 503, "body": ""},
            "https://fixture.invalid/": {
                "status": 200,
                "headers": {"content-type": "text/html"},
                "body": "<html><head><link rel='canonical' href='/page'><link rel='alternate' hreflang='fr' href='/page'><meta name='robots' content='index,follow'></head><body><a href='/page'>page</a><a href='/private'>private</a><a href='/training'>training</a></body></html>"
            },
            "https://fixture.invalid/page": {
                "status": 200,
                "headers": {"content-type": "text/html"},
                "body": "<html><head><link rel='canonical' href='/page'><link rel='alternate' hreflang='en' href='/'><link rel='alternate' hreflang='fr' href='/'><meta name='GPTBot' content='noindex'></head><body><h1>Page</h1></body></html>"
            },
            "https://fixture.invalid/hidden": {
                "status": 200,
                "headers": {"content-type": "text/html"},
                "body": "<html><head><link rel='canonical' href='/hidden'></head><body><h1>Hidden</h1></body></html>"
            },
            "https://fixture.invalid/training": {
                "status": 200,
                "headers": {"content-type": "text/html"},
                "body": "<html><head><link rel='canonical' href='/training'></head><body><h1>Training</h1></body></html>"
            }
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
    let id = request.request_id.to_string();
    let store = Store::open(db.to_str().unwrap()).await?;
    assert_eq!(store.view(&id).await?.unwrap().state, "local_only");
    assert!(store.report(&id).await?.is_none());
    store.0.close().await;
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
            "saved-index-integrity",
            &id,
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, std::fs::read(&db)?);
    assert!(!dir.path().join("saved-index.sqlite-wal").exists());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["command"], "saved-index-integrity");
    assert_eq!(result["completion"], "completed");
    assert_eq!(result["discovery_coverage"], "unknown");
    assert_eq!(result["freshness"]["state"], "run_interval_only");
    assert_eq!(result["coverage"]["complete"], false);
    assert_eq!(result["robots"]["state"], "observed");
    assert_eq!(result["legacy"]["report_present"], false);

    let pages = result["pages"].as_array().unwrap();
    let private = pages
        .iter()
        .find(|page| page["url"] == "https://fixture.invalid/private")
        .unwrap();
    assert_eq!(private["page"]["blocked_by_robots"], true);
    assert_eq!(private["evidence"]["capture_state"], "robots_blocked");
    assert_eq!(private["canonical_path"]["state"], "blocked");

    let training = pages
        .iter()
        .find(|page| page["url"] == "https://fixture.invalid/training")
        .unwrap();
    let training_gpt = training["effective_indexing"]
        .as_array()
        .unwrap()
        .iter()
        .find(|decision| decision["bot"] == "GPTBot")
        .unwrap();
    assert_eq!(training_gpt["crawl"], "denied");

    let page = pages
        .iter()
        .find(|page| page["url"] == "https://fixture.invalid/page")
        .unwrap();
    let gpt = page["effective_indexing"]
        .as_array()
        .unwrap()
        .iter()
        .find(|decision| decision["bot"] == "GPTBot")
        .unwrap();
    assert_eq!(gpt["index"], "restricted");
    assert!(!gpt["applicable_sources"].as_array().unwrap().is_empty());
    let google = page["effective_indexing"]
        .as_array()
        .unwrap()
        .iter()
        .find(|decision| decision["bot"] == "Googlebot")
        .unwrap();
    assert_eq!(google["index"], "allowed");
    assert_eq!(page["canonical"]["state"], "consistent");
    assert_eq!(page["canonical"]["target"], "https://fixture.invalid/page");

    let home = pages
        .iter()
        .find(|page| page["url"] == "https://fixture.invalid/")
        .unwrap();
    assert_eq!(home["canonical"]["state"], "consistent");
    assert_eq!(
        home["canonical_path"]["path"],
        serde_json::json!(["https://fixture.invalid/", "https://fixture.invalid/page"])
    );
    assert_eq!(home["canonical_path"]["state"], "terminal");

    let hidden = pages
        .iter()
        .find(|page| page["url"] == "https://fixture.invalid/hidden")
        .unwrap();
    let origins = hidden["sitemap_origins"]["roots"].as_array().unwrap();
    assert_eq!(origins.len(), 1);
    assert_eq!(origins[0]["paths"].as_array().unwrap().len(), 2);
    assert_eq!(
        origins[0]["paths"][0]["index_entry_ordinals"],
        serde_json::json!([0, 0])
    );
    assert_eq!(origins[0]["paths"][0]["lastmod"]["state"], "valid");

    let canonical = result["graphs"]["canonical"].as_array().unwrap();
    assert!(canonical.iter().any(|graph| {
        graph["source"] == "https://fixture.invalid/"
            && graph["state"] == "terminal"
            && graph["path"].as_array().unwrap().len() == 2
    }));
    let navigation = result["graphs"]["navigation"]["nodes"].as_array().unwrap();
    assert_eq!(
        navigation
            .iter()
            .find(|node| node["url"] == "https://fixture.invalid/page")
            .unwrap()["navigation_distance"],
        1
    );
    let findings = result["findings"].as_array().unwrap();
    for rule in [
        "page_blocked_by_robots",
        "crawl_denied",
        "index_restricted",
        "canonical_path_terminal",
        "sitemap_origin",
    ] {
        assert!(
            findings.iter().any(|finding| finding["rule"] == rule),
            "missing finding {rule}"
        );
    }
    for finding in findings {
        assert!(
            finding["path"].is_array()
                && finding["source"].is_object()
                && finding["url"].is_string(),
            "finding lacks explicit path/source/url: {finding}"
        );
    }

    // A pre-finalization terminal snapshot remains readable and unknown without
    // changing the stored bytes or the original report behavior.
    let legacy = dir.path().join("legacy.sqlite");
    std::fs::copy(&db, &legacy)?;
    let pool = SqlitePool::connect_with(SqliteConnectOptions::new().filename(&legacy)).await?;
    sqlx::query("ALTER TABLE jobs DROP COLUMN local_finalization")
        .execute(&pool)
        .await?;
    pool.close().await;
    let legacy_before = std::fs::read(&legacy)?;
    let legacy_output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            legacy.to_str().unwrap(),
            "saved-index-integrity",
            &id,
        ])
        .output()?;
    assert!(
        legacy_output.status.success(),
        "{}",
        String::from_utf8_lossy(&legacy_output.stderr)
    );
    let legacy_result: serde_json::Value = serde_json::from_slice(&legacy_output.stdout)?;
    assert_eq!(legacy_result["completion"], "unknown");
    assert_eq!(legacy_before, std::fs::read(&legacy)?);

    let missing = dir.path().join("missing.sqlite");
    let rejected = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            missing.to_str().unwrap(),
            "saved-index-integrity",
            &id,
        ])
        .output()?;
    assert!(!rejected.status.success());
    assert!(!missing.exists());

    let wal_db = dir.path().join("wal.sqlite");
    std::fs::copy(&db, &wal_db)?;
    std::fs::write(format!("{}-wal", wal_db.display()), "")?;
    let rejected = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            wal_db.to_str().unwrap(),
            "saved-index-integrity",
            &id,
        ])
        .output()?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("WAL"));

    let pre_mode = dir.path().join("pre-mode.sqlite");
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&pre_mode)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
    )
    .await?;
    sqlx::query("CREATE TABLE jobs (id TEXT PRIMARY KEY, request TEXT, state TEXT)")
        .execute(&pool)
        .await?;
    pool.close().await;
    let pre_before = std::fs::read(&pre_mode)?;
    let rejected = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            pre_mode.to_str().unwrap(),
            "saved-index-integrity",
            &id,
        ])
        .output()?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("legacy jobs schema"));
    assert_eq!(pre_before, std::fs::read(&pre_mode)?);
    Ok(())
}
