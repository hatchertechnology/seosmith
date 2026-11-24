//! Read-only saved-report export for bounded local schema-4 evaluation.
//!
//! This is not a worker or delivery path. It reads one already-saved report from
//! an immutable SQLite snapshot, normalizes a private copy to the base schema,
//! and runs the reviewed finding-coverage producer. The original row, raw
//! report, receipts and delivery state are not modified.
//!
//! Schema 1/2 reports keep their existing worker/outbox behavior. This command
//! only derives a local schema-4 view and never submits, delivers or enqueues
//! anything. A report carrying an evidence contract is refused because this
//! view does not carry or verify that evidence.

use anyhow::{Context, Result, ensure};
use seosmith::{
    contract::MAX_REPORT_BYTES,
    finding_coverage_prototype,
    model::{Report, digest},
    policy::AdvisoryPolicy,
};
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{io::Write, path::Path};

/// Derive a schema-4 finding-coverage view from a saved report.
///
/// The input is copied; the caller's report is not modified. A saved policy
/// configuration is reused when the caller does not supply one. Reports with an
/// evidence contract are refused so unavailable evidence is not silently
/// dropped from the derived view.
///
/// # Errors
/// Returns an error when the report carries an evidence contract, when the
/// producer rejects the base report, or when the derived bytes exceed the
/// report byte limit.
pub fn derive(report: &Report, policy: Option<&AdvisoryPolicy>) -> Result<Vec<u8>> {
    ensure!(
        report.evidence_contract.is_none(),
        "saved export does not accept evidence-contract reports"
    );
    let saved_policy = report
        .policy_guidance
        .as_ref()
        .map(|guidance| guidance.config.clone());
    let policy = policy.cloned().or(saved_policy).unwrap_or_default();
    let mut base = report.clone();
    base.schema_version = 1;
    base.policy_guidance = None;
    base.evidence_contract = None;
    let bytes = finding_coverage_prototype::generate(&mut base, &policy)?;
    ensure!(
        bytes.len() <= seosmith::delivery::HTP_MAX_REPORT_BYTES,
        "saved export exceeds the report byte limit"
    );
    Ok(bytes)
}

/// Read one saved report without writes and emit its derived schema-4 bytes.
///
/// # Errors
/// Returns an error when the database is absent, has a WAL or rollback journal,
/// lacks the saved report, stores a report larger than 16 MiB, or the derivation
/// fails.
pub async fn run(database: &Path, request_id: &str, policy: Option<&AdvisoryPolicy>) -> Result<()> {
    ensure!(database.is_file(), "database does not exist");
    // Immutable mode never creates SQLite sidecars. Refuse a live or
    // uncheckpointed WAL so a stale snapshot is never presented as current.
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    ensure!(
        !Path::new(&wal).exists(),
        "database has a WAL; close writers and checkpoint it before export"
    );
    let mut journal = database.as_os_str().to_os_string();
    journal.push("-journal");
    ensure!(
        !Path::new(&journal).exists(),
        "database has a rollback journal; use a quiesced snapshot"
    );
    let options = SqliteConnectOptions::new()
        .filename(database)
        .read_only(true)
        .immutable(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("open existing database read-only")?;
    let limit = i64::try_from(MAX_REPORT_BYTES).context("report limit does not fit SQLite")?;
    let row = sqlx::query(
        "SELECT CASE WHEN length(CAST(report AS BLOB)) <= ? THEN report END AS report, \
         length(CAST(report AS BLOB)) AS bytes FROM jobs WHERE id=?",
    )
    .bind(limit)
    .bind(request_id)
    .fetch_optional(&pool)
    .await
    .context("unsupported or legacy jobs schema")?
    .context("saved report absent")?;
    let stored: Option<String> = row.try_get("report")?;
    let stored_bytes: Option<i64> = row.try_get("bytes")?;
    let Some(stored) = stored else {
        if stored_bytes.is_some_and(|bytes| bytes > limit) {
            anyhow::bail!("saved report exceeds 16 MiB");
        }
        anyhow::bail!("saved report absent");
    };
    let report: Report =
        serde_json::from_str(&stored).context("saved report is not a valid report")?;
    let bytes = derive(&report, policy)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes).context("write saved export")?;
    stdout.flush().context("flush saved export")?;
    eprintln!("sha256={} bytes={}", digest(&bytes), bytes.len());
    Ok(())
}
