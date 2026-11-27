//! Regression: `Store::pending()` and `Store::pages()` must not need a temp directory.
//!
//! Thor wedged because `pending()` sorted rows that carried the whole `report`
//! TEXT. Above SQLite's sorter budget (about 1.77 MB) the sorter spills to a temp
//! file, and the pod has a read-only root and no `/tmp`: `SQLITE_IOERR_GETTEMPPATH`
//! (6410) on every delivery tick, forever (see `coordination/tasks/w1c-thor-repro.md`).
//!
//! `Store::pages()` (`ORDER BY depth,url` over the `page` TEXT, called by
//! `crawler::report` at the end of every crawl) spills the same way once the stored
//! page JSON passes about 2 MB, so the `temp_store=MEMORY` pragma, not the
//! `pending()` rewrite, is what protects it.
//!
//! The failure needs a process whose temp files cannot be written. Two independent
//! ways:
//! - `rlimit`: the test binary runs again under `sh -c 'ulimit -f ...'` (no
//!   `bwrap`, no namespaces). Every write past the limit fails, including a sorter
//!   spill, wherever SQLite looks for a temp directory. This is the required guard:
//!   it runs on every host and never skips.
//! - `bwrap`: `--ro-bind / /` with only the DB directory writable, which is thor's
//!   exact condition (`SQLITE_IOERR_GETTEMPPATH`, 6410). Extra coverage where
//!   `bwrap` is usable; it prints `SKIPPED` and returns otherwise.
use anyhow::{Result, ensure};
use seosmith::{model::CrawlRequest, store::Store};
use std::process::Command;

const CHILD_DB: &str = "SEOSMITH_PENDING_SPILL_DB";
const CHILD_MODE: &str = "SEOSMITH_PENDING_SPILL_MODE";
const PAGE_ROWS: usize = 150;
const PAGE_BYTES: usize = 20_000;
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

/// One job per `(report size, created)`, all pending delivery. `created` is explicit so
/// a test can insert the oldest row last (rowid order must not look like age order).
async fn seed(store: &Store, rows: &[(usize, i64)]) -> Result<()> {
    for (i, (size, created)) in rows.iter().enumerate() {
        let r = request(0x1000 + (i as u128) * 2)?;
        store.submit(&r).await?;
        let body = "x".repeat(*size);
        sqlx::query(
            "UPDATE jobs SET report=?, report_hash=?, state='completed', created=? WHERE id=?",
        )
        .bind(&body)
        .bind(seosmith::model::digest(body.as_bytes()))
        .bind(created)
        .bind(r.request_id.to_string())
        .execute(&store.0)
        .await?;
    }
    Ok(())
}

/// One job with `PAGE_ROWS` checked pages of about `PAGE_BYTES` each (3 MB of page JSON).
async fn seed_pages(store: &Store) -> Result<String> {
    let r = request(0x9000)?;
    store.submit(&r).await?;
    let id = r.request_id.to_string();
    for i in 0..PAGE_ROWS {
        let url = format!("https://spill.example/p{i}");
        let page = serde_json::json!({"url": url, "depth": i % 7, "title": "t".repeat(PAGE_BYTES)});
        sqlx::query("INSERT INTO frontier(job_id,url,depth,in_sitemap,page) VALUES(?,?,?,0,?)")
            .bind(&id)
            .bind(&url)
            .bind(i64::try_from(i % 7)?)
            .bind(page.to_string())
            .execute(&store.0)
            .await?;
    }
    Ok(id)
}

#[tokio::test]
async fn pending_returns_a_large_report_and_the_oldest_first() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("db.sqlite").to_str().unwrap()).await?;
    // The oldest row (created=1) is inserted last, so rowid order cannot pass for age order.
    seed(&store, &[(10, 5), (20, 3), (REPORT_BYTES, 1)]).await?;
    let (id, body, sha, attempts) = store.pending().await?.expect("a pending job");
    assert_eq!(id, uuid::Uuid::from_u128(0x1004).to_string());
    assert_eq!(body.len(), REPORT_BYTES);
    assert_eq!(sha, seosmith::model::digest(body.as_bytes()));
    assert_eq!(attempts, 0);
    Ok(())
}

#[tokio::test]
async fn pending_breaks_created_ties_by_id() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("db.sqlite").to_str().unwrap()).await?;
    // Same `created`; the larger id is inserted first.
    for id in [0x2002_u128, 0x2000] {
        let r = request(id)?;
        store.submit(&r).await?;
        sqlx::query(
            "UPDATE jobs SET report='r', report_hash='h', state='completed', created=7 WHERE id=?",
        )
        .bind(r.request_id.to_string())
        .execute(&store.0)
        .await?;
    }
    let (id, ..) = store.pending().await?.expect("a pending job");
    assert_eq!(id, uuid::Uuid::from_u128(0x2000).to_string());
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

/// Runs in the child process only; elsewhere it is a no-op.
#[tokio::test]
async fn child_reads_under_restricted_temp() -> Result<()> {
    let Some(db) = std::env::var_os(CHILD_DB) else {
        return Ok(());
    };
    let store = Store::open(db.to_str().unwrap()).await?;
    match std::env::var(CHILD_MODE).as_deref() {
        Ok("pending") => {
            let (_, body, _, _) = store.pending().await?.expect("a pending job");
            ensure!(body.len() == REPORT_BYTES, "wrong report: {} B", body.len());
            println!("CHILD-OK {}", body.len());
        }
        Ok("pages") => {
            let pages = store
                .pages(&request(0x9000)?.request_id.to_string())
                .await?;
            ensure!(
                pages.len() == PAGE_ROWS,
                "wrong page count: {}",
                pages.len()
            );
            ensure!(
                pages
                    .windows(2)
                    .all(|w| w[0].url <= w[1].url || w[0].depth < w[1].depth),
                "pages are not in depth,url order"
            );
            println!("CHILD-OK {}", pages.len());
        }
        other => anyhow::bail!("unknown child mode {other:?}"),
    }
    Ok(())
}

/// Seeds a database, then re-runs this binary on it through `wrap` (which receives
/// the command to run as its trailing arguments) and returns the combined output.
async fn run_child(
    mode: &str,
    wrap: impl FnOnce(&std::path::Path) -> Command,
) -> Result<(bool, String)> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("db.sqlite");
    let store = Store::open(db.to_str().unwrap()).await?;
    match mode {
        "pending" => seed(&store, &[(REPORT_BYTES, 1)]).await?,
        _ => {
            seed_pages(&store).await?;
        }
    }
    store.0.close().await;
    let mut command = wrap(dir.path());
    let output = command.env(CHILD_DB, &db).env(CHILD_MODE, mode).output()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), text))
}

fn child_args() -> Result<Vec<String>> {
    Ok(vec![
        std::env::current_exe()?.to_string_lossy().into_owned(),
        "--exact".into(),
        "child_reads_under_restricted_temp".into(),
        "--nocapture".into(),
    ])
}

/// No bwrap, no namespaces: writes past 256 KiB fail, so a 3 MB sorter spill cannot succeed
/// wherever SQLite puts its temp file. The database file itself is only read.
fn rlimit_command(_dir: &std::path::Path) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg("ulimit -f 256 && exec \"$0\" \"$@\"")
        .args(child_args().unwrap());
    c
}

#[tokio::test]
async fn pending_works_when_temp_files_cannot_be_written() -> Result<()> {
    let (ok, text) = run_child("pending", rlimit_command).await?;
    assert!(
        ok && text.contains("CHILD-OK 3000000"),
        "pending() failed:\n{text}"
    );
    Ok(())
}

#[tokio::test]
async fn pages_works_when_temp_files_cannot_be_written() -> Result<()> {
    let (ok, text) = run_child("pages", rlimit_command).await?;
    assert!(
        ok && text.contains("CHILD-OK 150"),
        "pages() failed:\n{text}"
    );
    Ok(())
}

fn bwrap_usable() -> bool {
    Command::new("bwrap")
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
        .output()
        .is_ok_and(|o| o.status.success())
}

fn bwrap_command(dir: &std::path::Path) -> Command {
    let dir_path = dir.to_str().unwrap();
    let mut c = Command::new("bwrap");
    c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"])
        // Only the database directory is writable, like thor's /data.
        .args(["--bind", dir_path, dir_path])
        .args(["--chdir", "/"])
        .args(child_args().unwrap());
    c
}

/// Thor's exact condition. Extra coverage; skipped with a message where bwrap is unusable.
#[tokio::test]
async fn pending_and_pages_work_with_no_writable_temp_directory() -> Result<()> {
    if !bwrap_usable() {
        eprintln!("SKIPPED: bwrap is not usable here; the rlimit tests still ran");
        return Ok(());
    }
    let (ok, text) = run_child("pending", bwrap_command).await?;
    assert!(
        ok && text.contains("CHILD-OK 3000000"),
        "pending() failed:\n{text}"
    );
    let (ok, text) = run_child("pages", bwrap_command).await?;
    assert!(
        ok && text.contains("CHILD-OK 150"),
        "pages() failed:\n{text}"
    );
    Ok(())
}
