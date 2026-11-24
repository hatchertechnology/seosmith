#![cfg(feature = "local-cli-fixtures")]

use seosmith::{model::Report, store::Store};
use std::{collections::BTreeMap, process::Command};
use uuid::Uuid;

#[tokio::test]
async fn local_cli_reopens_resource_references_without_delivery() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("local.sqlite");
    let mut request: Report =
        serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    request.request.request_id = Uuid::new_v4();
    request.request.url = "https://fixture.invalid/".parse()?;
    request.request.max_pages = 8;
    request.request.max_resources = 0;
    request.request.max_duration_seconds = 30;
    request.request.delay_ms = 1000;
    request.request.check_external_links = false;
    let id = request.request.request_id.to_string();
    let request_path = dir.path().join("request.json");
    let policy_path = dir.path().join("policy.json");
    let fixture_path = dir.path().join("fixtures.json");
    std::fs::write(&request_path, serde_json::to_vec(&request.request)?)?;
    std::fs::write(&policy_path, "{}")?;
    let fixtures = BTreeMap::from([
        (
            "https://fixture.invalid/robots.txt",
            serde_json::json!({"status":200,"body":"User-agent: *\nDisallow: /blocked\n"}),
        ),
        (
            "https://fixture.invalid/sitemap.xml",
            serde_json::json!({"status":404,"body":""}),
        ),
        (
            "https://fixture.invalid/",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html","content-length":"999"},"body":"<link rel='stylesheet' href='/style.css' media='screen' type='text/css'><picture><source srcset='small.webp 1x, big.webp 2x' media='(min-width: 10px)' type='image/webp'><img src='fallback.png' srcset='a.png 1x, b.png 2x' sizes='100vw'></picture><a href='/blocked'>blocked</a><a href='/plain'>plain</a><a href='/missing'>missing</a><a href='/large'>large</a><a href='/decode'>decode</a>"}),
        ),
        (
            "https://fixture.invalid/missing",
            serde_json::json!({"status":404,"headers":{"content-type":"text/html"},"body":"<img src='/missing.png'>"}),
        ),
        (
            "https://fixture.invalid/plain",
            serde_json::json!({"status":200,"headers":{"content-type":"text/plain"},"body":"text"}),
        ),
        (
            "https://fixture.invalid/large",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html"},"body":"x".repeat(256*1024+1)}),
        ),
        (
            "https://fixture.invalid/decode",
            serde_json::json!({"status":200,"headers":{"content-type":"text/html; charset=utf-16le"},"body":"x"}),
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = Store::open(db.to_str().unwrap()).await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT url,evidence FROM local_page_evidence WHERE job_id=?")
            .bind(&id)
            .fetch_all(&store.0)
            .await?;
    assert_eq!(rows.len(), 6);
    for (url, raw) in rows {
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let collectors = value["capture"]["collectors"].as_array().unwrap();
        assert_eq!(collectors[6]["collector"], "soft_404");
        let refs = &collectors[7];
        assert_eq!(refs["collector"], "resource_references");
        if url.ends_with("/blocked") {
            assert_eq!(refs["reason"], "blocked_by_robots");
        } else if url.ends_with("/missing") {
            assert_eq!(refs["state"], "unavailable");
            assert_eq!(refs["reason"], "unsuccessful_http_status");
        } else if url.ends_with("/plain") {
            assert_eq!(refs["reason"], "not_html");
        } else if url.ends_with("/large") {
            assert_eq!(refs["reason"], "source_byte_limit");
        } else if url.ends_with("/decode") {
            assert_eq!(refs["state"], "partial");
        } else {
            assert_eq!(refs["state"], "observed");
            let occurrences = refs["data"]["occurrences"].as_array().unwrap();
            assert_eq!(occurrences[0]["raw_rel"], "stylesheet");
            assert_eq!(occurrences[0]["media"], "screen");
            assert_eq!(occurrences[0]["declared_type"], "text/css");
            assert_eq!(occurrences[1]["raw_srcset"], "small.webp 1x, big.webp 2x");
            assert_eq!(occurrences[1]["media"], "(min-width: 10px)");
            assert_eq!(occurrences[1]["declared_type"], "image/webp");
            assert_eq!(occurrences[2]["raw_srcset"], "a.png 1x, b.png 2x");
            assert_eq!(collectors[2]["data"]["declared_content_length"], 999);
            assert!(collectors[2]["data"]["transfer_bytes"].is_null());
            assert_ne!(collectors[2]["data"]["decoded_body_bytes"], 999);
        }
    }
    assert_eq!(store.view(&id).await?.unwrap().state, "local_only");
    assert!(store.report(&id).await?.is_none());
    assert!(store.pending().await?.is_none());
    Ok(())
}
