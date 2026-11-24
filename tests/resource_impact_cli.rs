#![cfg(feature = "local-cli-fixtures")]

use seosmith::{model::Report, store::Store};
use std::{collections::BTreeMap, path::Path, process::Command};
use uuid::Uuid;

fn cli(database: &Path, id: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args([
            "--database",
            database.to_str().unwrap(),
            "resource-impact",
            id,
        ])
        .output()
        .unwrap()
}

async fn quiesce(database: &Path) -> anyhow::Result<()> {
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(database)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete),
    )
    .await?;
    pool.close().await;
    Ok(())
}

fn resource_entry<'a>(value: &'a serde_json::Value, url: &str) -> &'a serde_json::Value {
    value["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["url"]["url"] == url)
        .unwrap()
}

#[tokio::test]
async fn local_cli_joins_resource_measurements_with_page_provenance() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("local.sqlite");
    let report: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let mut request = report.request;
    request.request_id = Uuid::new_v4();
    request.url = "https://fixture.invalid/".parse()?;
    request.max_pages = 4;
    request.max_resources = 5;
    request.max_duration_seconds = 30;
    request.delay_ms = 1000;
    request.check_external_links = false;
    let id = request.request_id.to_string();
    let request_path = dir.path().join("request.json");
    let policy_path = dir.path().join("policy.json");
    let fixture_path = dir.path().join("fixtures.json");
    std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
    std::fs::write(&policy_path, "{}")?;
    let gif = "GIF89a\u{2}\u{0}\u{3}\u{0}padding";
    let fixtures = BTreeMap::from([
        (
            "https://fixture.invalid/robots.txt",
            serde_json::json!({"status":404,"body":""}),
        ),
        (
            "https://fixture.invalid/sitemap.xml",
            serde_json::json!({"status":404,"body":""}),
        ),
        (
            "https://fixture.invalid/",
            serde_json::json!({
                "status":200,
                "headers":{"content-type":"text/html"},
                "body":"<link rel='stylesheet' href='/style.css'><img src='/a.gif' srcset='/a.gif 1x, /b.gif 2x' width='2' height='3'><img src='/missing.gif'>"
            }),
        ),
        (
            "https://fixture.invalid/style.css",
            serde_json::json!({
                "status":200,
                "headers":{"content-type":"text/css"},
                "body":"body{background:url(/c.gif)}"
            }),
        ),
        (
            "https://fixture.invalid/a.gif",
            serde_json::json!({
                "status":200,
                "headers":{
                    "content-type":"image/gif",
                    "content-length":"999",
                    "content-encoding":"gzip",
                    "cache-control":"max-age=60",
                    "etag":"\"v1\"",
                    "last-modified":"yesterday"
                },
                "body":gif
            }),
        ),
        (
            "https://fixture.invalid/b.gif",
            serde_json::json!({
                "status":200,
                "headers":{"content-type":"image/gif"},
                "body":gif
            }),
        ),
        (
            "https://fixture.invalid/c.gif",
            serde_json::json!({
                "status":200,
                "headers":{"content-type":"image/gif"},
                "body":"GIF89a\u{1}\u{0}\u{1}\u{0}other"
            }),
        ),
        (
            "https://fixture.invalid/missing.gif",
            serde_json::json!({
                "status":404,
                "headers":{"content-type":"image/gif"},
                "body":"notfound"
            }),
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
    let rows: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT url,resource,page IS NOT NULL FROM frontier WHERE job_id=? ORDER BY url",
    )
    .bind(&id)
    .fetch_all(&store.0)
    .await?;
    assert_eq!(
        rows.iter()
            .filter(|(_, resource, fetched)| *resource && *fetched)
            .count(),
        5
    );
    let evidence: Vec<(String, String)> = sqlx::query_as(
        "SELECT f.url,e.evidence FROM frontier f JOIN local_page_evidence e ON e.job_id=f.job_id AND e.url=f.url WHERE f.job_id=? AND f.resource=1 ORDER BY f.url",
    )
    .bind(&id)
    .fetch_all(&store.0)
    .await?;
    assert_eq!(evidence.len(), 5);
    for (url, raw) in &evidence {
        let value: serde_json::Value = serde_json::from_str(raw)?;
        let resource = value["capture"]["collectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|collector| collector["collector"] == "resource")
            .unwrap();
        assert_eq!(resource["state"], "observed", "{url}");
        assert!(resource["data"]["transfer_bytes"].is_null(), "{url}");
    }
    drop(store);
    quiesce(&db).await?;
    assert!(!Path::new(&format!("{}-wal", db.display())).exists());
    let before = std::fs::read(&db)?;
    let output = cli(&db, &id);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, std::fs::read(&db)?, "analysis must not write");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["terminal_state"], "local_only");
    assert_eq!(value["completion"], "completed");
    assert_eq!(value["input_state"], "present");
    assert_eq!(value["coverage"]["requested_resources"], 5);
    assert_eq!(value["coverage"]["measured_resources"], 5);
    assert_eq!(value["coverage"]["unknown_measurement_resources"], 0);
    assert_eq!(value["coverage"]["unsuccessful_resources"], 1);
    assert_eq!(value["coverage"]["discovery_coverage"], "unknown");
    assert_eq!(value["bytes"]["counted_resources"], 4);
    assert_eq!(value["bytes"]["excluded_resources"], 1);
    assert_eq!(value["bytes"]["decoded_body_bytes"], 77);
    assert_eq!(value["bytes"]["complete"], false);

    let a = resource_entry(&value, "https://fixture.invalid/a.gif");
    assert_eq!(a["response"]["status"], 200);
    assert_eq!(a["bytes"]["decoded_body"]["value"], 17);
    assert_eq!(a["bytes"]["decoded_body"]["semantics"], "measured");
    assert_eq!(a["bytes"]["declared_content_length"]["value"], 999);
    assert_eq!(
        a["bytes"]["declared_content_length"]["semantics"],
        "estimated"
    );
    assert!(a["bytes"]["transfer"]["value"].is_null());
    assert_eq!(a["bytes"]["transfer"]["semantics"], "unknown");
    assert_eq!(a["mime"]["declared"]["value"], "image/gif");
    assert_eq!(a["mime"]["detected_signature"]["value"], "gif");
    assert_eq!(
        a["mime"]["image_dimensions"]["value"],
        serde_json::json!([2, 3])
    );
    assert_eq!(a["compression"]["content_encoding"]["value"], "gzip");
    assert!(a["compression"]["transport_decoded_body"]["value"].is_null());
    assert_eq!(a["cache"]["cache_control"]["value"], "max-age=60");
    assert_eq!(a["cache"]["etag"]["value"], "\"v1\"");
    assert_eq!(a["cache"]["last_modified"]["value"], "yesterday");
    assert_eq!(a["referrers"]["coverage"], "bounded");
    assert_eq!(
        a["affected_pages"]["pages"][0]["url"],
        "https://fixture.invalid/"
    );
    let referrers = a["referrers"]["items"].as_array().unwrap();
    assert!(referrers.iter().any(|referrer| {
        referrer["provenance"] == "static_html_reference"
            && referrer["attribute"] == "src"
            && referrer["page"]["url"] == "https://fixture.invalid/"
    }));
    assert!(referrers.iter().any(|referrer| {
        referrer["attribute"] == "srcset"
            && referrer["raw_value"] == "/a.gif"
            && referrer["candidate_descriptor"] == "1x"
    }));

    let b = resource_entry(&value, "https://fixture.invalid/b.gif");
    assert_eq!(a["body_sha256"]["value"], b["body_sha256"]["value"]);
    assert_eq!(value["duplicates"].as_array().unwrap().len(), 1);
    assert_eq!(value["duplicates"][0]["urls"].as_array().unwrap().len(), 2);
    let c = resource_entry(&value, "https://fixture.invalid/c.gif");
    assert_eq!(
        c["affected_pages"]["pages"][0]["url"],
        "https://fixture.invalid/"
    );
    let missing = resource_entry(&value, "https://fixture.invalid/missing.gif");
    assert_eq!(missing["response"]["status"], 404);
    assert_eq!(missing["bytes"]["decoded_body"]["value"], 8);
    assert_eq!(missing["counted_in_byte_totals"], false);
    assert_eq!(missing["exclusion_reason"], "unsuccessful_http_status");

    // Simulate a pre-PR43 row: keep the page and measurements that exist, but drop
    // the resource collector. Missing bytes must remain unknown, not zero.
    let store = Store::open(db.to_str().unwrap()).await?;
    let raw: String =
        sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=? AND url=?")
            .bind(&id)
            .bind("https://fixture.invalid/b.gif")
            .fetch_one(&store.0)
            .await?;
    let mut evidence: serde_json::Value = serde_json::from_str(&raw)?;
    evidence["capture"]["collectors"]
        .as_array_mut()
        .unwrap()
        .retain(|collector| collector["collector"] != "resource");
    sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
        .bind(serde_json::to_string(&evidence)?)
        .bind(&id)
        .bind("https://fixture.invalid/b.gif")
        .execute(&store.0)
        .await?;
    drop(store);
    quiesce(&db).await?;
    let before = std::fs::read(&db)?;
    let output = cli(&db, &id);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, std::fs::read(&db)?);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let b = resource_entry(&value, "https://fixture.invalid/b.gif");
    assert_eq!(b["measurement"]["state"], "absent");
    assert!(b["bytes"]["decoded_body"]["value"].is_null());
    assert_eq!(b["bytes"]["decoded_body"]["semantics"], "unknown");
    assert!(b["bytes"]["declared_content_length"]["value"].is_null());
    assert_eq!(b["counted_in_byte_totals"], false);
    assert_eq!(b["exclusion_reason"], "resource_measurement_unavailable");
    assert_eq!(value["coverage"]["unknown_measurement_resources"], 1);
    assert_eq!(value["bytes"]["decoded_body_bytes"], 60);
    assert_eq!(value["bytes"]["unknown_decoded_bytes_resources"], 1);
    assert_eq!(value["bytes"]["complete"], false);
    let gif_group = value["bytes"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["mime"] == "image/gif")
        .unwrap();
    assert_eq!(gif_group["counted_resources"], 2);
    assert_eq!(gif_group["decoded_body_bytes"], 32);
    assert_eq!(gif_group["unknown_decoded_bytes_resources"], 1);
    assert_eq!(gif_group["complete"], false);
    assert!(
        !value["bytes"]["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| group["decoded_body_bytes"] == 0)
    );
    assert!(value["duplicates"].as_array().unwrap().is_empty());
    assert_eq!(value["resources"].as_array().unwrap().len(), 5);
    assert!(row_present(&value, "https://fixture.invalid/b.gif"));
    Ok(())
}

fn row_present(value: &serde_json::Value, url: &str) -> bool {
    value["resources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["url"]["url"] == url)
}

#[tokio::test]
async fn resource_impact_accepts_legacy_finalization_and_refuses_unsafe_inputs()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("legacy.sqlite");
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true),
    )
    .await?;
    sqlx::raw_sql("CREATE TABLE jobs (id TEXT PRIMARY KEY, request TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued', started_at TEXT, report TEXT, report_hash TEXT, error TEXT, delivery_state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0, next_attempt INTEGER NOT NULL DEFAULT 0, discovery_truncated INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL DEFAULT (unixepoch()), execution_mode TEXT); CREATE TABLE local_page_evidence (job_id TEXT NOT NULL, url TEXT NOT NULL, evidence TEXT NOT NULL, PRIMARY KEY(job_id,url)); CREATE TABLE frontier (job_id TEXT NOT NULL, url TEXT NOT NULL, depth INTEGER NOT NULL, in_sitemap INTEGER NOT NULL, page TEXT, resource INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(job_id,url));")
        .execute(&pool)
        .await?;
    let report: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let mut request = report.request;
    request.request_id = Uuid::new_v4();
    request.url = "https://fixture.invalid/".parse()?;
    let id = request.request_id.to_string();
    let observed = "https://fixture.invalid/a.gif";
    let missing = "https://fixture.invalid/b.gif";
    let observed_page = seosmith::model::Page {
        url: observed.into(),
        status: Some(200),
        content_type: "image/gif".into(),
        bytes: 10,
        is_resource: true,
        ..seosmith::model::Page::default()
    };
    let observed_evidence = serde_json::json!({
        "capture": {
            "version": 1,
            "url": observed,
            "collectors": [{
                "collector": "resource",
                "version": 1,
                "state": "observed",
                "reason": null,
                "data": seosmith::resource_evidence::resource(
                    observed,
                    &BTreeMap::from([
                        ("content-type".to_string(), "image/gif".to_string()),
                        ("content-length".to_string(), "999".to_string()),
                    ]),
                    b"GIF89a\x02\x00\x03\x00",
                )
            }]
        }
    });
    for (url, page, evidence) in [
        (
            observed,
            Some(serde_json::to_string(&observed_page)?),
            Some(serde_json::to_string(&observed_evidence)?),
        ),
        (missing, None, None),
    ] {
        sqlx::query(
            "INSERT INTO frontier(job_id,url,depth,in_sitemap,page,resource) VALUES(?,?,0,0,?,1)",
        )
        .bind(&id)
        .bind(url)
        .bind(page)
        .execute(&pool)
        .await?;
        if let Some(evidence) = evidence {
            sqlx::query("INSERT INTO local_page_evidence(job_id,url,evidence) VALUES(?,?,?)")
                .bind(&id)
                .bind(url)
                .bind(evidence)
                .execute(&pool)
                .await?;
        }
    }
    sqlx::query(
        "INSERT INTO jobs(id,request,state,execution_mode) VALUES(?,?,'local_only','local')",
    )
    .bind(&id)
    .bind(serde_json::to_string(&request)?)
    .execute(&pool)
    .await?;
    pool.close().await;
    quiesce(&db).await?;
    let before = std::fs::read(&db)?;
    let output = cli(&db, &id);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(before, std::fs::read(&db)?);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["completion"], "unknown");
    assert_eq!(value["coverage"]["requested_resources"], 2);
    assert_eq!(value["coverage"]["measured_resources"], 1);
    assert_eq!(value["coverage"]["unknown_measurement_resources"], 1);
    assert_eq!(value["bytes"]["decoded_body_bytes"], 10);
    assert_eq!(value["bytes"]["unknown_decoded_bytes_resources"], 1);
    assert_eq!(value["bytes"]["complete"], false);
    let unknown_group = value["bytes"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["mime"].is_null())
        .unwrap();
    assert_eq!(unknown_group["counted_resources"], 0);
    assert!(unknown_group["decoded_body_bytes"].is_null());
    assert_eq!(unknown_group["unknown_decoded_bytes_resources"], 1);
    assert_eq!(unknown_group["complete"], false);
    let missing = resource_entry(&value, missing);
    assert!(missing["bytes"]["decoded_body"]["value"].is_null());
    assert_eq!(missing["exclusion_reason"], "resource_not_fetched");

    let wal = dir.path().join("legacy.sqlite-wal");
    std::fs::write(&wal, b"")?;
    let output = cli(&db, &id);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("WAL"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_file(&wal)?;
    let absent = dir.path().join("absent.sqlite");
    let output = cli(&absent, &id);
    assert!(!output.status.success());
    assert!(!absent.exists());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("database does not exist"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn resource_impact_refuses_non_local_jobs() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("standard.sqlite");
    let store = Store::open(db.to_str().unwrap()).await?;
    let report: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let mut request = report.request;
    request.request_id = Uuid::new_v4();
    store.submit(&request).await?;
    drop(store);
    quiesce(&db).await?;
    let output = cli(&db, &request.request_id.to_string());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not terminal local-only"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
