//! Regression: `Store::pending()` must not need a temp directory.
//!
//! Thor wedged because `pending()` sorted rows that carried the whole `report`
//! TEXT. Above SQLite's sorter budget (about 1.77 MB) the sorter spills to a temp
//! file, and the pod has a read-only root and no `/tmp`: `SQLITE_IOERR_GETTEMPPATH`
//! (6410) on every delivery tick, forever (see `coordination/tasks/w1c-thor-repro.md`).
//!
//! The failure needs a process with no writable temp directory, so the real
//! check runs this test binary again under `bwrap --ro-bind / /`. Where bwrap is
//! unavailable the sandboxed test says so and skips; the two checks that need
//! no sandbox still run everywhere.
use anyhow::{Result, ensure};
use seosmith::{model::CrawlRequest, store::Store};
use std::process::Command;

const CHILD_DB: &str = "SEOSMITH_PENDING_SPILL_DB";
const REPORT_BYTES: usize = 3_000_000;

fn request(id: u128) -> Result<CrawlRequest> {
    Ok(serde_json::from_value(serde_json::json!({
        "request_id": uuid::Uuid::from_u128(id),
        "website_id": uuid::Uuid::from_u128(id + 1),
        "binding_generation": 1,
        "url": "https://spill.example/",
        "max_pages": 5,
        "max_duration_seconds": 60,
        "delay_ms": 1000,
        "check_external_links": false,
        "max_resources": 0
    }))?)
}

/// One job per size, oldest first, all pending delivery.
async fn seed(store: &Store, sizes: &[usize]) -> Result<()> {
    for (i, size) in sizes.iter().enumerate() {
        let r = request(0x1000 + (i as u128) * 2)?;
        store.submit(&r).await?;
        let body = "x".repeat(*size);
        sqlx::query(
            "UPDATE jobs SET report=?, report_hash=?, state='completed', created=? WHERE id=?",
        )
        .bind(&body)
        .bind(seosmith::model::digest(body.as_bytes()))
        .bind(i64::try_from(i)?)
        .bind(r.request_id.to_string())
        .execute(&store.0)
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn pending_returns_a_large_report_and_the_oldest_first() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("db.sqlite").to_str().unwrap()).await?;
    seed(&store, &[REPORT_BYTES, 10, 20]).await?;
    let (id, body, sha, attempts) = store.pending().await?.expect("a pending job");
    assert_eq!(id, uuid::Uuid::from_u128(0x1000).to_string());
    assert_eq!(body.len(), REPORT_BYTES);
    assert_eq!(sha, seosmith::model::digest(body.as_bytes()));
    assert_eq!(attempts, 0);
    Ok(())
}

#[tokio::test]
async fn every_connection_keeps_sqlite_temp_data_in_memory() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("db.sqlite").to_str().unwrap()).await?;
    // Hold all four pooled connections at once: the setting must be per connection.
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(store.0.acquire().await?);
    }
    for conn in &mut held {
        let mode: i64 = sqlx::query_scalar("PRAGMA temp_store")
            .fetch_one(&mut **conn)
            .await?;
        assert_eq!(mode, 2, "temp_store must be MEMORY (2), got {mode}");
    }
    Ok(())
}

/// Runs inside the sandbox only; elsewhere it is a no-op.
#[tokio::test]
async fn sandboxed_child_reads_pending() -> Result<()> {
    let Some(db) = std::env::var_os(CHILD_DB) else {
        return Ok(());
    };
    let store = Store::open(db.to_str().unwrap()).await?;
    let (_, body, _, _) = store.pending().await?.expect("a pending job");
    ensure!(body.len() == REPORT_BYTES, "wrong report: {} B", body.len());
    println!("CHILD-OK {}", body.len());
    Ok(())
}

#[tokio::test]
async fn pending_works_with_no_writable_temp_directory() -> Result<()> {
    let probe = Command::new("bwrap")
        .args([
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "true",
        ])
        .output();
    if !probe.is_ok_and(|o| o.status.success()) {
        eprintln!("SKIPPED: bwrap is not usable here; the sandboxed check did not run");
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("db.sqlite");
    let store = Store::open(db.to_str().unwrap()).await?;
    seed(&store, &[REPORT_BYTES]).await?;
    drop(store);
    let dir_path = dir.path().to_str().unwrap();
    let output = Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"])
        // Only the database directory is writable, like thor's /data.
        .args(["--bind", dir_path, dir_path])
        .args(["--chdir", "/"])
        .env(CHILD_DB, &db)
        .arg(std::env::current_exe()?)
        .args(["--exact", "sandboxed_child_reads_pending", "--nocapture"])
        .output()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && text.contains("CHILD-OK 3000000"),
        "pending() failed without a temp directory:\n{text}"
    );
    Ok(())
}
