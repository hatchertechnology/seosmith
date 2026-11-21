//! Local, operator-scoped adapter. No transport or credential forwarding.
use crate::{
    crawler,
    model::{self, CrawlRequest},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use serde_json::{Value, json};
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

pub const REPORT_LIMIT: usize = 16 * 1024 * 1024;
const REQUEST_LIMIT: u64 = 16 * 1024;

#[derive(Subcommand)]
pub enum Operation {
    /// Detect this adapter; does not open a database or contact a service.
    Capabilities,
    /// Execute one explicit bounded request; an identical retry only returns status.
    Start {
        #[arg(long)]
        request: PathBuf,
    },
    Status {
        #[arg(long)]
        id: Uuid,
    },
    Cancel {
        #[arg(long)]
        id: Uuid,
    },
    Report {
        #[arg(long)]
        id: Uuid,
    },
    Export {
        #[arg(long)]
        id: Uuid,
        #[arg(long)]
        output: PathBuf,
    },
}

pub fn capabilities() -> Value {
    json!({
        "adapter_version": 1, "engine_version": model::ENGINE_VERSION,
        "report_schema_version": model::SCHEMA_VERSION,
        "ruleset_version": model::RULESET_VERSION, "transport": "local_cli",
        "operations": ["seosmith.start", "seosmith.status", "seosmith.cancel", "seosmith.report", "seosmith.export"],
        "limits": {"request_bytes": REQUEST_LIMIT, "report_bytes": REPORT_LIMIT,
            "pages": [1,20000], "resources": [0,20000], "duration_seconds": [10,7200], "delay_ms": [1000,60000]},
        "advanced": {"evidence_v2": false, "authorized_staging": false, "ci_baseline": false},
        "delivery": "disabled", "authentication": "local_os_account"
    })
}

fn request(path: &Path) -> Result<CrawlRequest> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(REQUEST_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= usize::try_from(REQUEST_LIMIT)?,
        "request_too_large"
    );
    let value: Value = serde_json::from_slice(&bytes).context("invalid_request_json")?;
    for field in [
        "max_pages",
        "max_resources",
        "max_duration_seconds",
        "delay_ms",
        "check_external_links",
    ] {
        ensure!(value.get(field).is_some(), "explicit_bounds_required");
    }
    let r: CrawlRequest = serde_json::from_value(value).context("invalid_request")?;
    r.validate().context("invalid_scope")?;
    ensure!(
        !r.request_id.is_nil() && !r.website_id.is_nil(),
        "non_nil_identity_required"
    );
    Ok(r)
}

fn lock_path(database: &Path) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(".lock");
    name.into()
}

fn worker_active(database: &Path) -> Result<Option<bool>> {
    let file = match File::open(lock_path(database)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(false)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(Some(true)),
        Err(e) => Err(e.into()),
    }
}

/// Refuse legacy workers before opening or recovering an adapter-owned database.
pub async fn reject_legacy_database(database: &Path) -> Result<()> {
    if !database.exists() {
        return Ok(());
    }
    let options = SqliteConnectOptions::new()
        .filename(database)
        .read_only(true)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(2));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let marked: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='operations_scope')",
    )
    .fetch_one(&pool)
    .await?;
    ensure!(!marked, "operations_database_legacy_command_refused");
    Ok(())
}

async fn existing(database: &Path, write: bool) -> Result<Store> {
    ensure!(database.is_file(), "run_not_found");
    let options = SqliteConnectOptions::new()
        .filename(database)
        .read_only(!write)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(2));
    let store = Store(
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?,
    );
    let version: i64 = sqlx::query_scalar("SELECT version FROM operations_scope")
        .fetch_one(&store.0)
        .await
        .context("not_an_operations_database")?;
    ensure!(version == 1, "unsupported_operations_database");
    if write {
        store.ensure_execution_mode().await?;
    }
    Ok(store)
}

async fn status(store: &Store, database: &Path, id: Uuid, own_worker: bool) -> Result<Value> {
    let view = store
        .view(&id.to_string())
        .await?
        .context("run_not_found")?;
    let active = if own_worker {
        Some(true)
    } else {
        worker_active(database)?
    };
    let unfinished = matches!(view.state.as_str(), "queued" | "running");
    let available: bool = sqlx::query_scalar("SELECT report IS NOT NULL FROM jobs WHERE id=?")
        .bind(id.to_string())
        .fetch_one(&store.0)
        .await?;
    Ok(
        json!({"adapter_version":1, "request_id":id, "state":view.state,
        "execution_state": if !unfinished {"terminal"} else if active == Some(true) {"active"} else {"unknown"},
        "worker_active": active, "report_available": available,
        "delivery_state":"disabled", "error": view.error}),
    )
}

// Check the database byte length before allocating report text; do not deserialize
// through Report, which could discard fields from a newer retained schema.
async fn report_bytes(store: &Store, id: Uuid) -> Result<String> {
    let row = sqlx::query("SELECT length(CAST(report AS BLOB)) AS size FROM jobs WHERE id=?")
        .bind(id.to_string())
        .fetch_optional(&store.0)
        .await?
        .context("run_not_found")?;
    let size: Option<i64> = row.try_get("size")?;
    let size = size.context("report_unavailable")?;
    ensure!(size <= i64::try_from(REPORT_LIMIT)?, "report_too_large");
    let report = store
        .report(&id.to_string())
        .await?
        .context("report_unavailable")?;
    ensure!(report.len() <= REPORT_LIMIT, "report_too_large");
    Ok(report)
}

async fn verify_retry(store: &Store, r: &CrawlRequest) -> Result<()> {
    let old: String = sqlx::query_scalar("SELECT request FROM jobs WHERE id=?")
        .bind(r.request_id.to_string())
        .fetch_optional(&store.0)
        .await?
        .context("worker_busy_use_status")?;
    ensure!(
        serde_json::from_str::<CrawlRequest>(&old)? == *r,
        "request_id_conflict"
    );
    Ok(())
}

async fn start(database: &Path, request_path: &Path) -> Result<Value> {
    let r = request(request_path)?;
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let lock = opts.open(lock_path(database))?;
    if let Err(error) = fs2::FileExt::try_lock_exclusive(&lock) {
        ensure!(
            error.kind() == std::io::ErrorKind::WouldBlock,
            "worker_lock_failed"
        );
        let store = existing(database, false).await?;
        verify_retry(&store, &r).await?;
        return status(&store, database, r.request_id, false).await;
    }
    let store = if database.exists() {
        existing(database, true).await?
    } else {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(database)?;
        let store = Store::open(database.to_str().context("invalid_database_path")?).await?;
        sqlx::query("CREATE TABLE operations_scope(version INTEGER NOT NULL)")
            .execute(&store.0)
            .await?;
        sqlx::query("INSERT INTO operations_scope VALUES(1)")
            .execute(&store.0)
            .await?;
        store
    };
    let other: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE id!=?")
        .bind(r.request_id.to_string())
        .fetch_one(&store.0)
        .await?;
    ensure!(other == 0, "database_scope_conflict_use_fresh_database");
    let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?)")
        .bind(r.request_id.to_string())
        .fetch_one(&store.0)
        .await?;
    if present {
        verify_retry(&store, &r).await?;
        // Dropping only our own guard makes interrupted work truthfully unknown.
        drop(lock);
        return status(&store, database, r.request_id, false).await;
    }
    store.submit(&r).await?;
    let job = store.claim().await?.context("run_not_queued")?;
    ensure!(job.request_id == r.request_id, "database_scope_conflict");
    run_bounded(&store, &r, crawler::crawl(&store, &r)).await?;
    drop(lock);
    status(&store, database, r.request_id, false).await
}

async fn run_bounded(
    store: &Store,
    r: &CrawlRequest,
    crawl: impl std::future::Future<Output = Result<model::Report>>,
) -> Result<()> {
    let result = tokio::time::timeout(Duration::from_secs(r.max_duration_seconds), crawl).await;
    match result {
        Ok(Ok(mut report)) => {
            // Serialize cancellation and finish to avoid reporting clean completion
            // when cancellation won the race with final report construction.
            let mut tx = store.0.begin_with("BEGIN IMMEDIATE").await?;
            let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id=?")
                .bind(r.request_id.to_string())
                .fetch_one(&mut *tx)
                .await?;
            if state == "cancelled" {
                report.outcome = "cancelled".into();
                report.stop_reason = "cancelled".into();
                report.coverage_complete = false;
            }
            let body = serde_json::to_string(&report)?;
            if body.len() > REPORT_LIMIT {
                tx.rollback().await?;
                store.fail(&r.request_id.to_string()).await?;
                anyhow::bail!("report_too_large");
            }
            sqlx::query("UPDATE jobs SET report=?,report_hash=?,state=? WHERE id=?")
                .bind(&body)
                .bind(model::digest(body.as_bytes()))
                .bind(&report.outcome)
                .bind(r.request_id.to_string())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        failure => {
            let error = if failure.is_err() {
                "adapter_deadline_exceeded"
            } else {
                "crawl_internal_error"
            };
            sqlx::query("UPDATE jobs SET state='failed',error=? WHERE id=? AND state='running'")
                .bind(error)
                .bind(r.request_id.to_string())
                .execute(&store.0)
                .await?;
        }
    }
    Ok(())
}

pub async fn execute(operation: Operation, database: &Path) -> Result<()> {
    if matches!(operation, Operation::Capabilities) {
        println!("{}", capabilities());
        return Ok(());
    }
    ensure!(
        database.is_absolute(),
        "explicit_absolute_database_required"
    );
    match operation {
        Operation::Capabilities => unreachable!(),
        Operation::Start { request } => println!("{}", start(database, &request).await?),
        Operation::Status { id } => {
            println!(
                "{}",
                status(&existing(database, false).await?, database, id, false).await?
            );
        }
        Operation::Cancel { id } => {
            let store = existing(database, true).await?;
            let accepted = store.cancel(&id.to_string()).await?;
            let mut result = status(&store, database, id, false).await?;
            result["cancel_accepted"] = json!(accepted);
            // The worker may still be unwinding; this is not a terminal ack.
            result["cancellation_terminal"] = json!(
                result["worker_active"].as_bool() == Some(false) && result["state"] == "cancelled"
            );
            println!("{result}");
        }
        Operation::Report { id } => {
            let body = report_bytes(&existing(database, false).await?, id).await?;
            std::io::stdout().lock().write_all(body.as_bytes())?;
        }
        Operation::Export { id, output } => {
            let body = report_bytes(&existing(database, false).await?, id).await?;
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(output)
                .context("export_destination_unavailable")?;
            file.write_all(body.as_bytes())?;
            file.sync_all()?;
            println!(
                "{}",
                json!({"request_id":id,"bytes":body.len(),"sha256":model::digest(body.as_bytes())})
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, Store, CrawlRequest) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("run.sqlite").to_str().unwrap())
            .await
            .unwrap();
        let r = CrawlRequest {
            request_id: Uuid::new_v4(),
            website_id: Uuid::new_v4(),
            binding_generation: 1,
            url: "https://example.com/".parse().unwrap(),
            max_pages: 1,
            max_resources: 0,
            max_duration_seconds: 10,
            delay_ms: 1000,
            check_external_links: false,
        };
        store.submit(&r).await.unwrap();
        store.claim().await.unwrap();
        (dir, store, r)
    }

    #[tokio::test]
    async fn whole_crawl_deadline_preserves_unknown_coverage_without_report() {
        let (_dir, store, r) = fixture().await;
        run_bounded(&store, &r, std::future::pending())
            .await
            .unwrap();
        let view = store
            .view(&r.request_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.state, "failed");
        assert_eq!(view.error.as_deref(), Some("adapter_deadline_exceeded"));
        assert!(
            store
                .report(&r.request_id.to_string())
                .await
                .unwrap()
                .is_none()
        );
        // Seed checkpoint survives and is not reported as a checked page.
        assert!(
            store
                .next(&r.request_id.to_string())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn cancellation_winning_finish_race_cannot_commit_clean_report() {
        let (_dir, store, r) = fixture().await;
        let report: model::Report = serde_json::from_value(json!({
            "schema_version": 1, "engine_version": "fixture", "ruleset_version": "fixture",
            "request": r, "started_at": "2026-11-16T00:00:00Z", "finished_at": "2026-11-16T00:00:01Z",
            "outcome": "completed", "stop_reason": "frontier_exhausted", "coverage_complete": true,
            "pages": [], "findings": []
        })).unwrap();
        let crawl = async {
            store.cancel(&r.request_id.to_string()).await?;
            Ok(report)
        };
        run_bounded(&store, &r, crawl).await.unwrap();
        let retained: Value = serde_json::from_str(
            &store
                .report(&r.request_id.to_string())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(retained["outcome"], "cancelled");
        assert_eq!(retained["coverage_complete"], false);
        assert!(!store.cancel(&r.request_id.to_string()).await.unwrap());
    }
}
