mod resource_impact_cli;
#[cfg(feature = "policy-wire-prototype")]
mod saved_export;
mod saved_graph;
mod saved_index_integrity;
mod saved_pair;
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
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    time::Duration,
};
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
    /// Emit production schema-4 (`s14-wire-1`) reports with S01 evidence. Requires
    /// `--advisory-policy`; incompatible with `--evidence-contract-v2`.
    #[arg(long, env = "SEOSMITH_POLICY_WIRE_V1", default_value_t = false)]
    policy_wire_v1: bool,
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
    /// Analyze retained checkpoints of a terminal local-only job, without writes.
    SavedGraph {
        request_id: uuid::Uuid,
    },
    /// Analyze saved local resource delivery and affected pages without writes.
    ResourceImpact {
        request_id: uuid::Uuid,
    },
    /// Compare two saved page assessments or two caller-supplied retained HTML files.
    SavedPair {
        /// Terminal local-only job ID for saved-checkpoint comparison.
        #[arg(long)]
        request_id: Option<uuid::Uuid>,
        /// Source page URL from the saved local job.
        #[arg(long)]
        source: Option<String>,
        /// Explicit comparison peer URL from the saved local job.
        #[arg(long)]
        peer: Option<String>,
        /// Caller-supplied retained source HTML file; no fetch is performed.
        #[arg(long)]
        source_html: Option<std::path::PathBuf>,
        /// Caller-supplied retained peer HTML file; no fetch is performed.
        #[arg(long)]
        peer_html: Option<std::path::PathBuf>,
        /// Response status asserted by the caller for --source-html.
        #[arg(long)]
        source_status: Option<u16>,
        /// Response status asserted by the caller for --peer-html.
        #[arg(long)]
        peer_status: Option<u16>,
        /// Source content type for --source-html.
        #[arg(long)]
        source_content_type: Option<String>,
        /// Peer content type for --peer-html.
        #[arg(long)]
        peer_content_type: Option<String>,
    },
    /// Explain saved effective indexing and canonical, hreflang and sitemap evidence.
    SavedIndexIntegrity {
        request_id: uuid::Uuid,
    },
    Rules,
    /// Validate a saved report locally without submitting or delivering a job.
    ValidateStructured {
        report: std::path::PathBuf,
    },
    /// Export one saved report as a bounded local schema-4 finding-coverage view without writes.
    #[cfg(feature = "policy-wire-prototype")]
    SavedExport {
        request_id: uuid::Uuid,
    },
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
        !(cli.policy_wire_v1 && cli.evidence_contract_v2),
        "the schema-4 wire carries its own evidence; disable SEOSMITH_EVIDENCE_CONTRACT_V2"
    );
    ensure!(
        !cli.policy_wire_v1 || cli.advisory_policy.is_some(),
        "--policy-wire-v1 requires --advisory-policy"
    );
    ensure!(
        !(cli.evidence_contract_v2 && cli.advisory_policy.is_some()),
        "S14 and S01 cannot be combined until their wire contract is agreed"
    );
    let policy: Option<seosmith::policy::AdvisoryPolicy> = cli
        .advisory_policy
        .as_ref()
        .map(|path| -> Result<_> {
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
        !(policy.is_some() && matches!(cli.command, Command::Serve { .. }) && !cli.policy_wire_v1),
        "S14 policy reports are delivered only with --policy-wire-v1"
    );
    if let Command::ValidateStructured { ref report } = cli.command {
        ensure!(
            policy.is_none() && !cli.evidence_contract_v2,
            "validation cannot be combined with experimental report contracts"
        );
        let mut input = Vec::new();
        std::fs::File::open(report)?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut input)?;
        ensure!(input.len() <= 16 * 1024 * 1024, "report exceeds 16 MiB");
        let source: serde_json::Value = serde_json::from_slice(&input)?;
        match source
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(1) => {}
            Some(2) => ensure!(
                source
                    .pointer("/evidence_contract/version")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1),
                "unsupported or missing evidence contract version"
            ),
            _ => anyhow::bail!("unsupported report schema version"),
        }
        if let Some(pages) = source.get("pages").and_then(serde_json::Value::as_array) {
            for (i, page) in pages.iter().enumerate() {
                ensure!(
                    page.get("structured_data")
                        .is_some_and(serde_json::Value::is_array),
                    "page {i} lacks a structured_data collection; source coverage is unavailable"
                );
            }
        }
        let report: seosmith::model::Report = serde_json::from_value(source)?;
        let limit = std::env::var("SEOSMITH_STRUCTURED_OUTPUT_LIMIT_BYTES")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("invalid SEOSMITH_STRUCTURED_OUTPUT_LIMIT_BYTES")?
            .unwrap_or(seosmith::structured_validation::MAX_REPORT_OUTPUT_BYTES);
        let output =
            seosmith::structured_validation::report_wrapper_with_limit(&report.pages, limit)?;
        std::io::stdout().write_all(&output.serialized)?;
        return Ok(());
    }
    #[cfg(feature = "policy-wire-prototype")]
    if let Command::SavedExport { request_id } = cli.command {
        ensure!(
            !cli.evidence_contract_v2,
            "saved export does not accept evidence contract v2"
        );
        saved_export::run(
            std::path::Path::new(&cli.database),
            &request_id.to_string(),
            policy.as_ref(),
        )
        .await?;
        return Ok(());
    }
    if let Command::SavedGraph { request_id } = cli.command {
        ensure!(
            policy.is_none() && !cli.evidence_contract_v2,
            "saved graph does not accept report options"
        );
        saved_graph::run(std::path::Path::new(&cli.database), &request_id.to_string()).await?;
        return Ok(());
    }
    if let Command::ResourceImpact { request_id } = cli.command {
        ensure!(
            policy.is_none() && !cli.evidence_contract_v2,
            "resource impact does not accept report options"
        );
        resource_impact_cli::run(std::path::Path::new(&cli.database), &request_id.to_string())
            .await?;
        return Ok(());
    }
    if let Command::SavedPair {
        request_id,
        source,
        peer,
        source_html,
        peer_html,
        source_status,
        peer_status,
        source_content_type,
        peer_content_type,
    } = &cli.command
    {
        ensure!(
            policy.is_none() && !cli.evidence_contract_v2,
            "saved pair does not accept report options"
        );
        saved_pair::run(
            std::path::Path::new(&cli.database),
            saved_pair::SavedPairArgs {
                request_id: *request_id,
                source: source.as_deref(),
                peer: peer.as_deref(),
                source_html: source_html.as_deref(),
                peer_html: peer_html.as_deref(),
                source_status: *source_status,
                peer_status: *peer_status,
                source_content_type: source_content_type.as_deref(),
                peer_content_type: peer_content_type.as_deref(),
            },
        )
        .await?;
        return Ok(());
    }
    if let Command::SavedIndexIntegrity { request_id } = cli.command {
        ensure!(
            policy.is_none() && !cli.evidence_contract_v2,
            "saved index integrity does not accept report options"
        );
        saved_index_integrity::run(std::path::Path::new(&cli.database), &request_id.to_string())
            .await?;
        return Ok(());
    }
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
        Command::Rules
        | Command::ValidateStructured { .. }
        | Command::SavedGraph { .. }
        | Command::ResourceImpact { .. }
        | Command::SavedPair { .. }
        | Command::SavedIndexIntegrity { .. } => {}
        #[cfg(feature = "policy-wire-prototype")]
        Command::SavedExport { .. } => {}
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
            let mut report = if policy.is_some() {
                local_crawl(&store, &job).await?
            } else {
                crawler::crawl(&store, &job).await?
            };
            let mut wire_bytes = None;
            if let Some(policy) = &policy {
                // Under --policy-wire-v1 the job stays local-only: this prints the exact
                // bytes the worker would store and deliver, and delivers nothing.
                let result = if cli.policy_wire_v1 {
                    seosmith::finding_coverage_prototype::generate_wire_v1(&mut report, policy)
                        .map(|bytes| wire_bytes = Some(bytes))
                } else {
                    seosmith::policy::apply_with_evidence(&mut report, policy)
                };
                if result.is_ok() {
                    store.finish_local_only(&job.request_id.to_string()).await?;
                } else {
                    store.fail(&job.request_id.to_string()).await?;
                }
                result?;
            } else if !finalize_job(&store, &mut report, cli.evidence_contract_v2, None).await? {
                anyhow::bail!("report finalization failed");
            }
            if let Some(bytes) = wire_bytes {
                std::io::stdout().write_all(&bytes)?;
            } else {
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
        Command::Serve { bind } => {
            let app = App {
                store: store.clone(),
                token: secret("SEOSMITH_API_TOKEN")?,
            };
            let ingest = std::env::var("HTP_INGEST_URL").context("HTP_INGEST_URL is required")?;
            let endpoint = service_url(&ingest, loopback_http_allowed(&ingest))?;
            let delivery = Delivery::new(endpoint, secret("HTP_INGEST_TOKEN")?)?;
            let (stop, mut stopped) = watch::channel(false);
            let crawl_store = store.clone();
            let worker_stop = stop.clone();
            let wire_policy = cli.policy_wire_v1.then(|| policy.clone()).flatten();
            let worker = tokio::spawn(async move {
                let result = process_jobs(
                    crawl_store,
                    &mut stopped,
                    cli.evidence_contract_v2,
                    wire_policy.as_ref(),
                )
                .await;
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
    wire_policy: Option<&seosmith::policy::AdvisoryPolicy>,
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
                finalize_job(&store, &mut report, evidence_contract_v2, wire_policy).await?;
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

// The optional fixture transport is compiled only for offline acceptance. It never
// falls back to the network and is only consulted by the local policy-mode CLI.
async fn local_crawl(store: &Store, request: &CrawlRequest) -> Result<seosmith::model::Report> {
    #[cfg(feature = "local-cli-fixtures")]
    if let Some(path) = std::env::var_os("SEOSMITH_LOCAL_FIXTURES") {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 1024 * 1024, "fixture file exceeds limit");
        let fixtures: std::collections::BTreeMap<String, FixtureResponse> =
            serde_json::from_slice(&bytes)?;
        return crawler::crawl_local_with(store, request, &FixtureTransport(fixtures)).await;
    }
    crawler::crawl_local(store, request).await
}

#[cfg(feature = "local-cli-fixtures")]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureResponse {
    status: u16,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
    body: String,
    #[serde(default)]
    header_fields: Option<Vec<(String, String)>>,
}
#[cfg(feature = "local-cli-fixtures")]
struct FixtureTransport(std::collections::BTreeMap<String, FixtureResponse>);
#[cfg(feature = "local-cli-fixtures")]
impl seosmith::fetch::Transport for FixtureTransport {
    #[allow(clippy::unused_async_trait_impl)] // Matches the production transport's async interface.
    async fn get(&self, url: &url::Url) -> Result<seosmith::fetch::Response> {
        let fixture = self.0.get(url.as_str()).context("fixture URL missing")?;
        let mut fields = reqwest::header::HeaderMap::new();
        let entries: Vec<_> = fixture.header_fields.clone().unwrap_or_else(|| {
            fixture
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        });
        for (name, value) in entries {
            fields.append(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
                reqwest::header::HeaderValue::from_bytes(value.as_bytes())?,
            );
        }
        let (headers, occurrences) = seosmith::fetch::extract_headers(&fields);
        Ok(seosmith::fetch::Response {
            status: fixture.status,
            headers,
            local_http_occurrences: Some(occurrences),
            body: fixture.body.as_bytes().to_vec(),
            ttfb_ms: 0,
        })
    }
}

/// Test-only escape hatch for plain-HTTP delivery to a loopback IP literal
/// (`127.0.0.1` or `::1`). Always false unless built with `local-e2e`, which does
/// not compile in release builds.
fn loopback_http_allowed(value: &str) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    cfg!(feature = "local-e2e")
        && url::Url::parse(value).is_ok_and(|u| match u.host() {
            Some(url::Host::Ipv4(ip)) => ip == Ipv4Addr::LOCALHOST,
            Some(url::Host::Ipv6(ip)) => ip == Ipv6Addr::LOCALHOST,
            _ => false,
        })
}

async fn fail_logged(store: &Store, id: &str) {
    if let Err(error) = store.fail(id).await {
        tracing::error!(run_id=%id, %error, "could not mark the job failed");
    }
}

/// Finalise one crawled job. Every generation, cap, equality and evidence error is
/// converted to a failed job here and returns `Ok(false)`: `process_jobs`
/// propagates `Err` with `?`, which would stop the worker.
async fn finalize_job(
    store: &Store,
    report: &mut seosmith::model::Report,
    v2: bool,
    wire_policy: Option<&seosmith::policy::AdvisoryPolicy>,
) -> Result<bool> {
    if let Some(policy) = wire_policy {
        let id = report.request.request_id.to_string();
        let bytes = match seosmith::finding_coverage_prototype::generate_wire_v1(report, policy) {
            Ok(bytes) => bytes,
            Err(error) => {
                fail_logged(store, &id).await;
                tracing::error!(run_id=%id, %error, "schema-4 generation failed");
                return Ok(false);
            }
        };
        // Nothing below may return `Err`: that would stop the worker (m-g).
        return Ok(
            match store.finish_bytes(&id, &report.outcome, &bytes).await {
                Ok(()) => true,
                Err(error) => {
                    if !error.is::<seosmith::store::ReportTooLarge>()
                        && store.cancelled(&id).await.unwrap_or(false)
                    {
                        return Ok(false);
                    }
                    fail_logged(store, &id).await;
                    tracing::error!(run_id=%id, %error, "schema-4 persistence failed");
                    false
                }
            },
        );
    }
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
    async fn wire_generation_failure_fails_the_job_and_the_next_job_finalizes() -> Result<()> {
        use seosmith::finding_coverage_prototype::fixture_input;
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("w.sqlite").to_str().unwrap()).await?;
        let policy = seosmith::policy::AdvisoryPolicy::default();
        let mut oversized = fixture_input("complete-missing-h1")?;
        oversized.pages[0].title = "x".repeat(5_000_000);
        let mut ok = fixture_input("complete-missing-h1")?;
        for report in [&mut oversized, &mut ok] {
            report.request.request_id = uuid::Uuid::new_v4();
            store.submit(&report.request).await?;
            store.claim().await?.unwrap();
        }
        // The cap error is a failed job (`Ok(false)`), never an `Err` that would stop the worker.
        assert!(!finalize_job(&store, &mut oversized, false, Some(&policy)).await?);
        let failed = store
            .view(&oversized.request.request_id.to_string())
            .await?
            .unwrap();
        assert_eq!(failed.state, "failed");
        // The cause is a size cap (string, report-size or byte limit), not another generation error.
        let mut again = seosmith::finding_coverage_prototype::fixture_input("complete-missing-h1")?;
        again.pages[0].title = "x".repeat(5_000_000);
        let cause = seosmith::finding_coverage_prototype::generate_wire_v1(&mut again, &policy)
            .unwrap_err()
            .to_string();
        assert!(cause.contains("limit") || cause.contains("size"), "{cause}");
        assert!(store.pending().await?.is_none());
        assert!(finalize_job(&store, &mut ok, false, Some(&policy)).await?);
        let (id, body, sha, _) = store.pending().await?.unwrap();
        assert_eq!(id, ok.request.request_id.to_string());
        assert_eq!(sha, seosmith::model::digest(body.as_bytes()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body)?["schema_version"],
            4
        );
        Ok(())
    }

    #[test]
    fn loopback_http_is_only_for_ip_literals_under_local_e2e() {
        let enabled = cfg!(feature = "local-e2e");
        assert_eq!(loopback_http_allowed("http://127.0.0.1:3000/x"), enabled);
        assert_eq!(loopback_http_allowed("http://[::1]:3000/x"), enabled);
        for refused in [
            "http://localhost:3000/x",
            "http://127.0.0.2/x",
            "http://10.0.0.1/x",
            "http://example.com/x",
            "not a url",
        ] {
            assert!(!loopback_http_allowed(refused), "{refused}");
        }
    }

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
            let origin: Option<String> =
                sqlx::query_scalar("SELECT local_finalization FROM jobs WHERE id=?")
                    .bind(request.request_id.to_string())
                    .fetch_one(&store.0)
                    .await?;
            assert_eq!(origin.as_deref(), Some("interrupted"));
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
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(db)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::raw_sql("CREATE TABLE jobs (id TEXT PRIMARY KEY, request TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued', started_at TEXT, report TEXT, report_hash TEXT, error TEXT, delivery_state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0, next_attempt INTEGER NOT NULL DEFAULT 0, discovery_truncated INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL DEFAULT (unixepoch())); CREATE TABLE frontier (job_id TEXT NOT NULL REFERENCES jobs(id), url TEXT NOT NULL, depth INTEGER NOT NULL, in_sitemap INTEGER NOT NULL, page TEXT, resource INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(job_id,url));")
            .execute(&pool).await?;
        pool.close().await;
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
        assert!(!finalize_job(&store, &mut bad, false, None).await?);
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
        assert!(finalize_job(&store, &mut good, false, None).await?);
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
        assert!(
            finalize_job(&store, &mut report, false, None)
                .await
                .is_err()
        );
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
        assert!(finalize_job(&store, &mut report, false, None).await?);
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
        finalize_job(&store, &mut bad, true, None).await?;
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
        finalize_job(&store, &mut good, true, None).await?;
        assert!(
            store
                .report(&good.request.request_id.to_string())
                .await?
                .is_some()
        );
        Ok(())
    }
}
