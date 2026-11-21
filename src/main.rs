use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use seosmith::{
    api::{self, App},
    crawler,
    delivery::Delivery,
    fetch::service_url,
    model::CrawlRequest,
    store::Store,
};
use std::{fs::OpenOptions, time::Duration};
use tokio::sync::watch;
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, env = "SEOSMITH_DB", default_value = "data/seosmith.sqlite")]
    database: String,
    #[arg(long, env = "SEOSMITH_EVIDENCE_CONTRACT_V2", default_value_t = false)]
    evidence_contract_v2: bool,
    /// Experimental S14 policy JSON; requires compatible H04 display before activation.
    #[arg(long, env = "SEOSMITH_ADVISORY_POLICY")]
    advisory_policy: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Bounded standalone operations for agent hosts.
    Operations {
        #[command(subcommand)]
        operation: seosmith::operations::Operation,
    },
    Serve {
        #[arg(long, env = "SEOSMITH_BIND", default_value = "127.0.0.1:8080")]
        bind: String,
    },
    Crawl {
        #[arg(long)]
        request: std::path::PathBuf,
    },
    Rules,
}
fn secret(name: &str) -> Result<String> {
    let value = if let Ok(path) = std::env::var(format!("{name}_FILE")) {
        std::fs::read_to_string(path)?
    } else {
        std::env::var(name).with_context(|| format!("{name} or {name}_FILE is required"))?
    };
    let value = value.trim().to_owned();
    ensure!(value.len() >= 32, "{name} must have at least 32 characters");
    Ok(value)
}
async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "seosmith=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    ensure!(
        !(cli.evidence_contract_v2 && cli.advisory_policy.is_some()),
        "S14 and S01 cannot be combined until their wire contract is agreed"
    );
    let policy: Option<seosmith::policy::AdvisoryPolicy> = cli
        .advisory_policy
        .as_ref()
        .map(|path| -> Result<_> {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(65_537)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 65_536, "advisory policy exceeds 64 KiB");
            let policy: seosmith::policy::AdvisoryPolicy = serde_json::from_slice(&bytes)?;
            policy.validate()?;
            Ok(policy)
        })
        .transpose()?;
    ensure!(
        !(policy.is_some() && matches!(cli.command, Command::Serve { .. })),
        "S14 policy reports cannot be delivered until H04 and receiver support are ready"
    );
    if let Command::Operations { operation } = cli.command {
        if policy.is_some() {
            eprintln!(
                "{}",
                serde_json::json!({"error":"operation_failed", "reason":"advisory_policy_unsupported"})
            );
            std::process::exit(1);
        }
        if cli.evidence_contract_v2 {
            eprintln!(
                "{}",
                serde_json::json!({"error":"operation_failed", "reason":"evidence_v2_unsupported"})
            );
            std::process::exit(1);
        }
        if let Err(error) =
            seosmith::operations::execute(operation, std::path::Path::new(&cli.database)).await
        {
            // No error chains or backend bodies: request data and credentials must
            // never become diagnostics. Detailed backend errors stay internal.
            eprintln!(
                "{}",
                serde_json::json!({"error":"operation_failed", "reason":error.to_string()})
            );
            std::process::exit(1);
        }
        return Ok(());
    }
    if matches!(cli.command, Command::Rules) {
        ensure!(
            policy.is_none(),
            "advisory policy guidance is available in local policy output"
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&seosmith::audit::rules())?
        );
        return Ok(());
    }
    if let Some(parent) = std::path::Path::new(&cli.database).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(format!("{}.lock", cli.database))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("another Seosmith process owns this database")?;
    seosmith::operations::reject_legacy_database(std::path::Path::new(&cli.database)).await?;
    let store = Store::open(&cli.database).await?;
    store.recover().await?;
    match cli.command {
        Command::Operations { .. } => unreachable!(),
        Command::Rules => {}
        Command::Crawl { request } => {
            let r: CrawlRequest = serde_json::from_slice(&tokio::fs::read(request).await?)?;

            let job = if policy.is_some() {
                store.submit_local(&r).await?;
                store.claim_local(&r.request_id.to_string()).await?
            } else {
                store.submit(&r).await?;
                store.claim().await?
            }
            .context("no queued job; use report API for completed runs")?;
            ensure!(
                job.request_id == r.request_id,
                "database contains another queued job; use serve to drain it"
            );
            let mut report = crawler::crawl(&store, &job).await?;
            if let Some(policy) = &policy {
                let result = seosmith::policy::apply_with_evidence(&mut report, policy);
                if result.is_ok() {
                    store.finish_local_only(&job.request_id.to_string()).await?;
                } else {
                    store.fail(&job.request_id.to_string()).await?;
                }
                result?;
            } else if !finalize_job(&store, &mut report, cli.evidence_contract_v2).await? {
                anyhow::bail!("report finalization failed");
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Serve { bind } => {
            let app = App {
                store: store.clone(),
                token: secret("SEOSMITH_API_TOKEN")?,
            };
            let endpoint = service_url(
                &std::env::var("HTP_INGEST_URL").context("HTP_INGEST_URL is required")?,
                false,
            )?;
            let delivery = Delivery::new(endpoint, secret("HTP_INGEST_TOKEN")?)?;
            let (stop, mut stopped) = watch::channel(false);
            let crawl_store = store.clone();
            let worker_stop = stop.clone();
            let worker = tokio::spawn(async move {
                let result =
                    process_jobs(crawl_store, &mut stopped, cli.evidence_contract_v2).await;
                let _ = worker_stop.send(true);
                result
            });
            let push_store = store.clone();
            let mut push_stop = stop.subscribe();
            let push = tokio::spawn(async move {
                loop {
                    if *push_stop.borrow() {
                        break;
                    }
                    if delivery.tick(&push_store).await.is_err() {
                        tracing::error!("delivery queue unavailable");
                    }
                    if push_store.prune().await.is_err() {
                        tracing::error!("retention failed");
                    }
                    tokio::select! {()=tokio::time::sleep(Duration::from_secs(5))=>{},_=push_stop.changed()=>break}
                }
            });
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            tracing::info!(%bind,"Seosmith ready");
            let mut service_stop = stop.subscribe();
            axum::serve(listener, api::router(app))
                .with_graceful_shutdown(async move {
                    tokio::select! { () = shutdown() => {}, _ = service_stop.changed() => {} }

                    let _ = stop.send(true);
                })
                .await?;
            worker.await??;
            push.await?;
        }
    }
    Ok(())
}

async fn process_jobs(
    store: Store,
    stopped: &mut watch::Receiver<bool>,
    evidence_contract_v2: bool,
) -> Result<()> {
    loop {
        if *stopped.borrow() {
            return Ok(());
        }
        if let Some(request) = store.claim().await? {
            tracing::info!(run_id=%request.request_id, "crawl started");
            let result = tokio::select! {
                result = crawler::crawl(&store, &request) => result,
                _ = stopped.changed() => return Ok(()),
            };
            if let Ok(mut report) = result {
                finalize_job(&store, &mut report, evidence_contract_v2).await?;
            } else {
                store.fail(&request.request_id.to_string()).await?;
                tracing::error!(run_id=%request.request_id, "crawl failed");
            }
        } else {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(2)) => {},
                _ = stopped.changed() => return Ok(()),
            }
        }
    }
}

async fn finalize_job(
    store: &Store,
    report: &mut seosmith::model::Report,
    v2: bool,
) -> Result<bool> {
    if v2 && let Err(error) = seosmith::contract::enable_v2(report) {
        store.fail(&report.request.request_id.to_string()).await?;
        tracing::error!(run_id=%report.request.request_id, %error, "evidence finalization failed");
        return Ok(false);
    }
    match store.finish(report).await {
        Ok(()) => Ok(true),
        Err(error) if error.is::<seosmith::store::ReportTooLarge>() => {
            store.fail(&report.request.request_id.to_string()).await?;
            tracing::error!(run_id=%report.request.request_id, %error, "report finalization failed");
            Ok(false)
        }
        Err(error) => {
            if store
                .cancelled(&report.request.request_id.to_string())
                .await?
            {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_jobs_never_recover_into_delivery() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("jobs.sqlite");
        let store = Store::open(path.to_str().unwrap()).await?;
        let report: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut queued = report.request.clone();
        queued.request_id = uuid::Uuid::new_v4();
        let mut running = report.request.clone();
        running.request_id = uuid::Uuid::new_v4();
        store.submit_local(&queued).await?;
        store.submit_local(&running).await?;
        store
            .claim_local(&running.request_id.to_string())
            .await?
            .unwrap();
        let mut page = report.pages[0].clone();
        page.url = running.url.to_string();
        store
            .save_page(&running.request_id.to_string(), &page)
            .await?;
        assert_eq!(store.pages(&running.request_id.to_string()).await?.len(), 1);
        let mut local_report = report.clone();
        local_report.request = running.clone();
        assert!(store.finish(&local_report).await.is_err());
        drop(store);
        let store = Store::open(path.to_str().unwrap()).await?;
        store.recover().await?;
        for request in [&queued, &running] {
            assert_eq!(
                store
                    .view(&request.request_id.to_string())
                    .await?
                    .unwrap()
                    .state,
                "local_only"
            );
            assert!(store.submit(request).await.is_err());
        }
        assert!(store.claim().await?.is_none());
        assert!(store.pending().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn recovery_quarantines_ambiguous_jobs_but_requeues_standard() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("jobs.sqlite").to_str().unwrap()).await?;
        let report: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut old = report.request.clone();
        old.request_id = uuid::Uuid::new_v4();
        store.submit(&old).await?;
        sqlx::query("UPDATE jobs SET execution_mode=NULL WHERE id=?")
            .bind(old.request_id.to_string())
            .execute(&store.0)
            .await?;
        let mut standard = report.request.clone();
        standard.request_id = uuid::Uuid::new_v4();
        store.submit(&standard).await?;
        // Claim the standard request directly to avoid relying on queue ordering.
        sqlx::query("UPDATE jobs SET state='running' WHERE id=?")
            .bind(standard.request_id.to_string())
            .execute(&store.0)
            .await?;
        store.recover().await?;
        let view = store.view(&old.request_id.to_string()).await?.unwrap();
        assert_eq!(view.state, "failed");
        assert_eq!(view.error.as_deref(), Some("ambiguous_pre_mode_job"));
        assert_eq!(
            store.claim().await?.unwrap().request_id,
            standard.request_id
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_open_serializes_execution_mode_migration() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("jobs.sqlite");
        let db = path.to_str().unwrap();
        let store = Store::open(db).await?;
        // Refresh SQLite schema metadata before constructing a pre-mode database.
        sqlx::query("PRAGMA table_info(jobs)")
            .fetch_all(&store.0)
            .await?;
        sqlx::query("ALTER TABLE jobs DROP COLUMN execution_mode")
            .execute(&store.0)
            .await
            .context("drop pre-mode column")?;
        drop(store);
        let (a, b) = tokio::join!(Store::open(db), Store::open(db));
        a.context("first reopen")?;
        b.context("second reopen")?;
        Ok(())
    }

    #[tokio::test]
    async fn pending_rejects_local_even_if_report_exists() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("jobs.sqlite").to_str().unwrap()).await?;
        let report: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        store.submit_local(&report.request).await?;
        sqlx::query("UPDATE jobs SET report='{}',report_hash='unexpected' WHERE id=?")
            .bind(report.request.request_id.to_string())
            .execute(&store.0)
            .await?;
        assert!(store.pending().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn pre_mode_delivered_tombstone_remains_idempotent() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("jobs.sqlite").to_str().unwrap()).await?;
        let report: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        store.submit(&report.request).await?;
        store.claim().await?.unwrap();
        store.finish(&report).await?;
        let (id, _, hash, _) = store.pending().await?.unwrap();
        store.delivered(&id, &hash).await?;
        sqlx::query("UPDATE jobs SET execution_mode=NULL,created=unixepoch()-2592001 WHERE id=?")
            .bind(&id)
            .execute(&store.0)
            .await?;
        store.prune().await?;
        assert!(store.report(&id).await?.is_none());
        store.recover().await?;
        assert!(!store.submit(&report.request).await?);
        assert!(store.claim().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_or_finalized_jobs_cannot_be_written() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("jobs.sqlite").to_str().unwrap()).await?;
        let mut report: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        report.request.request_id = uuid::Uuid::new_v4();
        store.submit(&report.request).await?;
        store.claim().await?.unwrap();
        store.cancel(&report.request.request_id.to_string()).await?;
        assert!(store.finish(&report).await.is_err());
        assert!(store.pending().await?.is_none());
        report.request.request_id = uuid::Uuid::new_v4();
        store.submit(&report.request).await?;
        store.claim().await?.unwrap();
        store.finish(&report).await?;
        let before = store.pending().await?.unwrap();
        assert!(store.finish(&report).await.is_err());
        assert_eq!(store.pending().await?.unwrap(), before);
        Ok(())
    }

    #[tokio::test]
    async fn oversized_job_fails_and_next_job_survives_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("jobs.sqlite");
        let store = Store::open(path.to_str().unwrap()).await?;
        let mut bad: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        bad.request.request_id = uuid::Uuid::new_v4();
        store.submit(&bad.request).await?;
        store.claim().await?.unwrap();
        bad.pages[0]
            .structured_data
            .push(seosmith::model::StructuredData {
                format: "x".repeat(17 * 1024 * 1024),
                data: None,
                error: None,
            });
        assert!(!finalize_job(&store, &mut bad, false).await?);
        drop(store);
        let store = Store::open(path.to_str().unwrap()).await?;
        store.recover().await?;
        assert_eq!(
            store
                .view(&bad.request.request_id.to_string())
                .await?
                .unwrap()
                .state,
            "failed"
        );
        assert!(store.claim().await?.is_none());
        let mut good: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        good.request.request_id = uuid::Uuid::new_v4();
        store.submit(&good.request).await?;
        store.claim().await?.unwrap();
        assert!(finalize_job(&store, &mut good, false).await?);
        let (_, body, hash, _) = store.pending().await?.unwrap();
        assert_eq!(hash, seosmith::model::digest(body.as_bytes()));
        assert_eq!(body, serde_json::to_string(&good)?);
        Ok(())
    }

    #[tokio::test]
    async fn storage_error_is_not_report_rejection() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("jobs.sqlite").to_str().unwrap()).await?;
        let mut report: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        report.request.request_id = uuid::Uuid::new_v4();
        store.submit(&report.request).await?;
        store.claim().await?.unwrap();
        sqlx::query("CREATE TRIGGER reject_finish BEFORE UPDATE OF report ON jobs BEGIN SELECT RAISE(FAIL, 'storage unavailable'); END").execute(&store.0).await?;
        assert!(finalize_job(&store, &mut report, false).await.is_err());
        assert_eq!(
            store
                .view(&report.request.request_id.to_string())
                .await?
                .unwrap()
                .state,
            "running"
        );
        sqlx::query("DROP TRIGGER reject_finish")
            .execute(&store.0)
            .await?;
        store.recover().await?;
        assert!(store.claim().await?.is_some());
        assert!(finalize_job(&store, &mut report, false).await?);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_v2_job_fails_and_next_job_completes_after_recovery() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("jobs.sqlite").to_str().unwrap()).await?;
        let mut bad: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        bad.request.request_id = uuid::Uuid::new_v4();
        store.submit(&bad.request).await?;
        store.claim().await?.unwrap();
        let mut value = serde_json::json!(null);
        for _ in 0..64 {
            value = serde_json::json!([value]);
        }
        bad.pages[0]
            .structured_data
            .push(seosmith::model::StructuredData {
                format: "json_ld".into(),
                data: Some(value),
                error: None,
            });
        finalize_job(&store, &mut bad, true).await?;
        let id = bad.request.request_id.to_string();
        assert_eq!(store.view(&id).await?.unwrap().state, "failed");
        assert!(store.report(&id).await?.is_none());
        store.recover().await?;
        assert!(store.claim().await?.is_none());
        let mut good: seosmith::model::Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        good.request.request_id = uuid::Uuid::new_v4();
        store.submit(&good.request).await?;
        store.claim().await?.unwrap();
        finalize_job(&store, &mut good, true).await?;
        assert!(
            store
                .report(&good.request.request_id.to_string())
                .await?
                .is_some()
        );
        Ok(())
    }
}
