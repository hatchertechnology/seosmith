use crate::model::{CrawlRequest, Report, digest};
use anyhow::{Context, Result, ensure};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};
type FrontierCheckpoint = (Option<String>, Option<String>, i64, bool, bool);

#[derive(Clone)]
pub struct Store(pub SqlitePool);

#[derive(Debug)]
pub struct ReportTooLarge;
impl std::fmt::Display for ReportTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "report exceeds delivery limit")
    }
}
impl std::error::Error for ReportTooLarge {}
#[derive(serde::Serialize)]
pub struct JobView {
    pub request_id: String,
    pub state: String,
    pub delivery_state: String,
    pub attempts: i64,
    pub error: Option<String>,
}
impl Store {
    pub async fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(10));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY, request TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued',
            started_at TEXT, report TEXT, report_hash TEXT, error TEXT,
            delivery_state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0,
            next_attempt INTEGER NOT NULL DEFAULT 0, discovery_truncated INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL DEFAULT (unixepoch()));
            CREATE TABLE IF NOT EXISTS local_sitemap_run (job_id TEXT PRIMARY KEY REFERENCES jobs(id), evidence TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS local_robots_run (job_id TEXT PRIMARY KEY REFERENCES jobs(id), evidence TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS local_page_evidence (job_id TEXT NOT NULL REFERENCES jobs(id), url TEXT NOT NULL, evidence TEXT NOT NULL, PRIMARY KEY(job_id,url));
            CREATE TABLE IF NOT EXISTS local_evidence_usage (job_id TEXT PRIMARY KEY REFERENCES jobs(id), evidence_bytes INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS frontier (job_id TEXT NOT NULL REFERENCES jobs(id), url TEXT NOT NULL,
            depth INTEGER NOT NULL, in_sitemap INTEGER NOT NULL, page TEXT, PRIMARY KEY(job_id,url));")
            .execute(&pool).await?;
        let columns = sqlx::query("PRAGMA table_info(frontier)")
            .fetch_all(&pool)
            .await?;
        if !columns
            .iter()
            .any(|r| r.get::<String, _>("name") == "resource")
        {
            sqlx::query("ALTER TABLE frontier ADD COLUMN resource INTEGER NOT NULL DEFAULT 0")
                .execute(&pool)
                .await?;
        }
        let store = Self(pool);
        store.ensure_execution_mode().await?;
        Ok(store)
    }
    pub async fn ensure_execution_mode(&self) -> Result<()> {
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        let job_columns = sqlx::query("PRAGMA table_info(jobs)")
            .fetch_all(&mut *tx)
            .await?;
        if !job_columns
            .iter()
            .any(|r| r.get::<String, _>("name") == "execution_mode")
        {
            sqlx::query("ALTER TABLE jobs ADD COLUMN execution_mode TEXT")
                .execute(&mut *tx)
                .await?;
        }
        if !job_columns
            .iter()
            .any(|r| r.get::<String, _>("name") == "local_finalization")
        {
            sqlx::query("ALTER TABLE jobs ADD COLUMN local_finalization TEXT")
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn recover(&self) -> Result<()> {
        sqlx::query("UPDATE jobs SET state='failed',error='ambiguous_pre_mode_job' WHERE execution_mode IS NULL AND state IN ('queued','running')")
            .execute(&self.0).await?;
        sqlx::query(
            "UPDATE jobs SET state='queued' WHERE state='running' AND execution_mode='standard'",
        )
        .execute(&self.0)
        .await?;
        sqlx::query(
            "UPDATE jobs SET state='local_only',local_finalization=CASE WHEN execution_mode='local' THEN 'interrupted' ELSE NULL END WHERE state IN ('local_queued','local_running')",
        )
        .execute(&self.0)
        .await?;
        Ok(())
    }
    pub async fn submit(&self, r: &CrawlRequest) -> Result<bool> {
        self.submit_mode(r, false).await
    }
    pub async fn submit_local(&self, r: &CrawlRequest) -> Result<bool> {
        self.submit_mode(r, true).await
    }
    async fn submit_mode(&self, r: &CrawlRequest, local: bool) -> Result<bool> {
        r.validate()?;
        let id = r.request_id.to_string();
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        if let Some((old, old_mode, old_report, old_delivery, old_hash)) = sqlx::query_as::<
            _,
            (
                String,
                Option<String>,
                Option<String>,
                String,
                Option<String>,
            ),
        >(
            "SELECT request,execution_mode,report,delivery_state,report_hash FROM jobs WHERE id=?",
        )
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        {
            ensure!(
                serde_json::from_str::<CrawlRequest>(&old)? == *r,
                "request_id conflict"
            );
            ensure!(
                !local
                    && (old_mode.as_deref() == Some("standard")
                        || (old_mode.is_none()
                            && (old_report.is_some()
                                || (old_delivery == "delivered" && old_hash.is_some())))),
                "request_id belongs to another or unknown execution mode; use a new request ID"
            );
            return Ok(false);
        }
        let n:i64=sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state IN ('queued','running','local_queued','local_running') OR (report IS NOT NULL AND delivery_state NOT IN ('delivered','rejected'))").fetch_one(&mut *tx).await?;
        ensure!(n < 100, "queue full");
        sqlx::query("INSERT INTO jobs(id,request,state,execution_mode) VALUES(?,?,?,?)")
            .bind(&id)
            .bind(serde_json::to_string(r)?)
            .bind(if local { "local_queued" } else { "queued" })
            .bind(if local { "local" } else { "standard" })
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO frontier(job_id,url,depth,in_sitemap) VALUES(?,?,0,0)")
            .bind(&id)
            .bind(r.url.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }
    pub async fn claim(&self) -> Result<Option<CrawlRequest>> {
        let value:Option<String>=sqlx::query_scalar("UPDATE jobs SET state='running', started_at=coalesce(started_at,strftime('%Y-%m-%dT%H:%M:%fZ','now')) WHERE id=(SELECT id FROM jobs WHERE state='queued' AND execution_mode='standard' ORDER BY created,id LIMIT 1) RETURNING request").fetch_optional(&self.0).await?;
        value
            .map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub async fn claim_local(&self, id: &str) -> Result<Option<CrawlRequest>> {
        let value: Option<String> = sqlx::query_scalar("UPDATE jobs SET state='local_running', started_at=coalesce(started_at,strftime('%Y-%m-%dT%H:%M:%fZ','now')) WHERE id=? AND state='local_queued' AND execution_mode='local' RETURNING request")
            .bind(id)
            .fetch_optional(&self.0)
            .await?;
        value
            .map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub async fn cancelled(&self, id: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id=?")
                .bind(id)
                .fetch_one(&self.0)
                .await?
                == "cancelled",
        )
    }
    pub async fn cancel(&self, id: &str) -> Result<bool> {
        Ok(sqlx::query(
            "UPDATE jobs SET state='cancelled' WHERE id=? AND state IN ('queued','running','local_queued','local_running')",
        )
        .bind(id)
        .execute(&self.0)
        .await?
        .rows_affected()
            > 0)
    }
    pub async fn discover(
        &self,
        id: &str,
        url: &str,
        depth: usize,
        sitemap: bool,
        cap: usize,
    ) -> Result<()> {
        self.discover_kind(id, url, depth, sitemap, cap, false)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn discover_kind(
        &self,
        id: &str,
        url: &str,
        depth: usize,
        sitemap: bool,
        cap: usize,
        resource: bool,
    ) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO frontier(job_id,url,depth,in_sitemap,resource) SELECT ?,?,?,?,? WHERE (SELECT count(*) FROM frontier WHERE job_id=?) < ?")
            .bind(id).bind(url).bind(i64::try_from(depth)?).bind(sitemap).bind(resource).bind(id).bind(i64::try_from(cap)?).execute(&self.0).await?;
        sqlx::query("UPDATE jobs SET discovery_truncated=1 WHERE id=? AND NOT EXISTS(SELECT 1 FROM frontier WHERE job_id=? AND url=?)")
            .bind(id).bind(id).bind(url).execute(&self.0).await?;
        sqlx::query("UPDATE frontier SET in_sitemap=max(in_sitemap,?),resource=CASE WHEN page IS NULL THEN min(resource,?) ELSE resource END WHERE job_id=? AND url=?")
            .bind(sitemap).bind(resource).bind(id).bind(url).execute(&self.0).await?;
        Ok(())
    }
    pub async fn next_budgeted(
        &self,
        id: &str,
        pages: bool,
        resources: bool,
    ) -> Result<Option<(String, usize, bool, bool)>> {
        let row=sqlx::query("SELECT url,depth,in_sitemap,resource FROM frontier WHERE job_id=? AND page IS NULL AND ((resource=0 AND ?) OR (resource=1 AND ?)) ORDER BY resource,depth,url LIMIT 1")
            .bind(id).bind(pages).bind(resources).fetch_optional(&self.0).await?;
        row.map(|r| {
            Ok((
                r.try_get("url")?,
                usize::try_from(r.try_get::<i64, _>("depth")?)?,
                r.try_get("in_sitemap")?,
                r.try_get("resource")?,
            ))
        })
        .transpose()
    }
    pub async fn discovery_truncated(&self, id: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar("SELECT discovery_truncated FROM jobs WHERE id=?")
                .bind(id)
                .fetch_one(&self.0)
                .await?,
        )
    }
    pub async fn next(&self, id: &str) -> Result<Option<(String, usize, bool)>> {
        let row=sqlx::query("SELECT url,depth,in_sitemap FROM frontier WHERE job_id=? AND page IS NULL ORDER BY depth,url LIMIT 1").bind(id).fetch_optional(&self.0).await?;
        row.map(|r| {
            Ok((
                r.try_get("url")?,
                usize::try_from(r.try_get::<i64, _>("depth")?)?,
                r.try_get("in_sitemap")?,
            ))
        })
        .transpose()
    }
    pub async fn save_page(&self, id: &str, p: &crate::model::Page) -> Result<()> {
        sqlx::query("UPDATE frontier SET page=? WHERE job_id=? AND url=?")
            .bind(serde_json::to_string(p)?)
            .bind(id)
            .bind(&p.url)
            .execute(&self.0)
            .await?;
        Ok(())
    }
    pub(crate) async fn require_local_running(&self, request: &CrawlRequest) -> Result<()> {
        let stored: Option<String> = sqlx::query_scalar("SELECT request FROM jobs WHERE id=? AND execution_mode='local' AND state='local_running'").bind(request.request_id.to_string()).fetch_optional(&self.0).await?;
        ensure!(
            stored
                .as_deref()
                .map(serde_json::from_str::<CrawlRequest>)
                .transpose()?
                .as_ref()
                == Some(request),
            "job is not the claimed local request"
        );
        Ok(())
    }
    /// Replace the bounded discovery snapshot while the local job is running.
    pub(crate) async fn record_local_sitemap(
        &self,
        id: &str,
        run: &crate::sitemap_evidence::LocalRun,
    ) -> Result<()> {
        let value = serde_json::to_string(run)?;
        ensure!(
            value.len() <= 2 * 1024 * 1024,
            "sitemap evidence exceeds limit"
        );
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        let state: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT state,execution_mode FROM jobs WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        ensure!(
            state == Some(("local_running".into(), Some("local".into()))),
            "job is not local running"
        );
        let existing: Option<String> =
            sqlx::query_scalar("SELECT evidence FROM local_sitemap_run WHERE job_id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(old) = existing {
            if old == value || run.state == "in_progress" {
                tx.commit().await?;
                return Ok(());
            }
            let old: crate::sitemap_evidence::LocalRun = serde_json::from_str(&old)?;
            ensure!(
                old.state == "in_progress" && run.state != "in_progress",
                "sitemap evidence replay differs"
            );
        }
        sqlx::query("INSERT INTO local_sitemap_run(job_id,evidence) VALUES(?,?) ON CONFLICT(job_id) DO UPDATE SET evidence=excluded.evidence")
            .bind(id).bind(value).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Persist bounded origin outcomes even if no page is checkpointed. Serialized with cancellation.
    pub(crate) async fn record_local_robots(
        &self,
        id: &str,
        origin: &str,
        source: &str,
    ) -> Result<()> {
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        let state: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT state,execution_mode FROM jobs WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        ensure!(
            state == Some(("local_running".into(), Some("local".into()))),
            "job is not local running"
        );
        let existing: Option<String> =
            sqlx::query_scalar("SELECT evidence FROM local_robots_run WHERE job_id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let mut value = match existing {
            Some(s) => serde_json::from_str::<serde_json::Value>(&s)?,
            None => serde_json::json!({"version":1,"origins":[],"omitted_origins":0}),
        };
        let origins = value["origins"]
            .as_array_mut()
            .context("invalid robots run evidence")?;
        if origins.len() < 32 && origin.len() <= 1024 {
            origins.push(serde_json::json!({"origin":origin,"source":source}));
        } else {
            let omitted = value["omitted_origins"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(1);
            value["omitted_origins"] = omitted.into();
        }
        sqlx::query("INSERT INTO local_robots_run(job_id,evidence) VALUES(?,?) ON CONFLICT(job_id) DO UPDATE SET evidence=excluded.evidence").bind(id).bind(serde_json::to_string(&value)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Atomically persist a local page and its fixed collector set. Replays must match bytes.
    pub(crate) async fn checkpoint_local(
        &self,
        id: &str,
        page: &crate::model::Page,
        evidence: &serde_json::Value,
    ) -> Result<()> {
        let page_bytes = serde_json::to_string(page)?;
        let input_bytes = serde_json::to_string(evidence)?;
        let input_hash = digest(input_bytes.as_bytes());
        let mut evidence_bytes = serde_json::to_string(
            &serde_json::json!({"input_hash": input_hash, "capture": evidence}),
        )?;
        ensure!(
            crate::local_checkpoint::url_identity_matches(evidence, &page.url),
            "evidence URL mismatch"
        );
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        let state: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT state,execution_mode FROM jobs WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        ensure!(
            state == Some(("local_running".into(), Some("local".into()))),
            "job is not local running"
        );
        let existing: Option<FrontierCheckpoint> = sqlx::query_as("SELECT f.page,e.evidence,f.depth,f.in_sitemap,f.resource FROM frontier f LEFT JOIN local_page_evidence e ON e.job_id=f.job_id AND e.url=f.url WHERE f.job_id=? AND f.url=?").bind(id).bind(&page.url).fetch_optional(&mut *tx).await?;
        let (old_page, old_evidence, depth, in_sitemap, resource) =
            existing.context("page is not in frontier")?;
        if old_page.is_some() || old_evidence.is_some() {
            ensure!(
                old_page.as_deref() == Some(page_bytes.as_str())
                    && old_evidence
                        .as_deref()
                        .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                        .and_then(|v| v
                            .get("input_hash")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned))
                        .as_deref()
                        == Some(input_hash.as_str()),
                "checkpoint replay conflict"
            );
            tx.commit().await?;
            return Ok(());
        }
        ensure!(
            i64::try_from(page.depth)? == depth
                && page.in_sitemap == in_sitemap
                && page.is_resource == resource,
            "page metadata differs from frontier"
        );
        let used: Option<i64> =
            sqlx::query_scalar("SELECT evidence_bytes FROM local_evidence_usage WHERE job_id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let used = if let Some(used) = used {
            used
        } else {
            // Older stores have no counter. Initialize it once without rewriting evidence.
            let used: i64 = sqlx::query_scalar("SELECT coalesce(sum(length(CAST(evidence AS BLOB))),0) FROM local_page_evidence WHERE job_id=?").bind(id).fetch_one(&mut *tx).await?;
            sqlx::query("INSERT INTO local_evidence_usage(job_id,evidence_bytes) VALUES(?,?)")
                .bind(id)
                .bind(used)
                .execute(&mut *tx)
                .await?;
            used
        };
        // Reserve room for one bounded omission record per permitted page/resource.
        if used
            .checked_add(i64::try_from(evidence_bytes.len())?)
            .context("evidence byte count overflow")?
            > 16 * 1024 * 1024
        {
            evidence_bytes = serde_json::to_string(
                &serde_json::json!({"input_hash": input_hash, "capture": crate::local_checkpoint::budget_omission(&page.url)}),
            )?;
        }
        sqlx::query("UPDATE frontier SET page=? WHERE job_id=? AND url=? AND page IS NULL")
            .bind(&page_bytes)
            .bind(id)
            .bind(&page.url)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO local_page_evidence(job_id,url,evidence) VALUES(?,?,?)")
            .bind(id)
            .bind(&page.url)
            .bind(&evidence_bytes)
            .execute(&mut *tx)
            .await?;
        let total = used
            .checked_add(i64::try_from(evidence_bytes.len())?)
            .context("evidence byte count overflow")?;
        sqlx::query("UPDATE local_evidence_usage SET evidence_bytes=? WHERE job_id=?")
            .bind(total)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn pages(&self, id: &str) -> Result<Vec<crate::model::Page>> {
        let rows=sqlx::query("SELECT page,in_sitemap FROM frontier WHERE job_id=? AND page IS NOT NULL ORDER BY depth,url").bind(id).fetch_all(&self.0).await?;
        rows.into_iter()
            .map(|r| {
                let mut p: crate::model::Page =
                    serde_json::from_str(&r.try_get::<String, _>("page")?)?;
                p.in_sitemap = r.try_get("in_sitemap")?;
                Ok(p)
            })
            .collect()
    }
    pub async fn started(&self, id: &str) -> Result<String> {
        Ok(sqlx::query_scalar("SELECT started_at FROM jobs WHERE id=?")
            .bind(id)
            .fetch_one(&self.0)
            .await?)
    }
    pub async fn finish(&self, r: &Report) -> Result<()> {
        ensure!(
            matches!(r.schema_version, 1 | 2),
            "unsupported or local-only schema cannot be persisted for delivery"
        );
        let mode: Option<String> = sqlx::query_scalar("SELECT execution_mode FROM jobs WHERE id=?")
            .bind(r.request.request_id.to_string())
            .fetch_optional(&self.0)
            .await?
            .flatten();
        ensure!(
            mode.as_deref() == Some("standard"),
            "local or ambiguous job cannot be persisted for delivery"
        );
        let body = serde_json::to_string(r)?;
        if body.len() > 16 * 1024 * 1024 {
            return Err(ReportTooLarge.into());
        }
        let updated = sqlx::query("UPDATE jobs SET report=?,report_hash=?,state=? WHERE id=? AND state='running' AND execution_mode='standard' AND report IS NULL")
            .bind(&body)
            .bind(digest(body.as_bytes()))
            .bind(&r.outcome)
            .bind(r.request.request_id.to_string())
            .execute(&self.0)
            .await?.rows_affected();
        ensure!(updated == 1, "job is not running or already finalized");
        Ok(())
    }
    /// Persist already-serialised schema-4 bytes exactly as generated. The stored
    /// text and `report_hash` are the delivered bytes and their digest; nothing
    /// is re-serialised. `finish` stays the only path for schema 1/2.
    pub async fn finish_bytes(&self, id: &str, outcome: &str, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() <= crate::delivery::HTP_MAX_REPORT_BYTES,
            ReportTooLarge
        );
        let header: serde_json::Value = serde_json::from_slice(bytes)?;
        ensure!(
            header
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                == Some(4)
                && header
                    .pointer("/request/request_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(id),
            "finish_bytes persists only schema-4 bytes of this job"
        );
        ensure!(
            header
                .pointer("/evidence_contract/version")
                .and_then(serde_json::Value::as_u64)
                == Some(1),
            "finish_bytes requires the schema-4 evidence contract (version 1)"
        );
        let body = std::str::from_utf8(bytes)?;
        let mode: Option<String> = sqlx::query_scalar("SELECT execution_mode FROM jobs WHERE id=?")
            .bind(id)
            .fetch_optional(&self.0)
            .await?
            .flatten();
        ensure!(
            mode.as_deref() == Some("standard"),
            "local or ambiguous job cannot be persisted for delivery"
        );
        let updated = sqlx::query("UPDATE jobs SET report=?,report_hash=?,state=? WHERE id=? AND state='running' AND execution_mode='standard' AND report IS NULL")
            .bind(body)
            .bind(digest(bytes))
            .bind(outcome)
            .bind(id)
            .execute(&self.0)
            .await?.rows_affected();
        ensure!(updated == 1, "job is not running or already finalized");
        Ok(())
    }
    pub async fn finish_local_only(&self, id: &str) -> Result<()> {
        let updated = sqlx::query(
            "UPDATE jobs SET state='local_only',error=NULL,local_finalization='completed' WHERE id=? AND state='local_running' AND execution_mode='local'",
        )
        .bind(id)
        .execute(&self.0)
        .await?.rows_affected();
        ensure!(updated == 1, "local job was not running at finalization");
        Ok(())
    }
    pub async fn fail(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE jobs SET state='failed',error='crawl_internal_error' WHERE id=? AND state IN ('running','local_running')").bind(id).execute(&self.0).await?;
        Ok(())
    }
    pub async fn view(&self, id: &str) -> Result<Option<JobView>> {
        let row = sqlx::query("SELECT state,delivery_state,attempts,error FROM jobs WHERE id=?")
            .bind(id)
            .fetch_optional(&self.0)
            .await?;
        row.map(|r| {
            Ok(JobView {
                request_id: id.into(),
                state: r.try_get("state")?,
                delivery_state: r.try_get("delivery_state")?,
                attempts: r.try_get("attempts")?,
                error: r.try_get("error")?,
            })
        })
        .transpose()
    }
    pub async fn report(&self, id: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar::<_, Option<String>>("SELECT report FROM jobs WHERE id=?")
                .bind(id)
                .fetch_optional(&self.0)
                .await?
                .flatten(),
        )
    }
    pub async fn pending(&self) -> Result<Option<(String, String, String, i64)>> {
        let row=sqlx::query("SELECT id,report,report_hash,attempts FROM jobs WHERE report IS NOT NULL AND (execution_mode='standard' OR execution_mode IS NULL) AND delivery_state='pending' AND next_attempt<=unixepoch() ORDER BY created LIMIT 1").fetch_optional(&self.0).await?;
        row.map(|r| {
            Ok((
                r.try_get("id")?,
                r.try_get("report")?,
                r.try_get("report_hash")?,
                r.try_get("attempts")?,
            ))
        })
        .transpose()
    }
    pub async fn delivered(&self, id: &str, sha: &str) -> Result<()> {
        let n = sqlx::query(
            "UPDATE jobs SET delivery_state='delivered',error=NULL WHERE id=? AND report_hash=?",
        )
        .bind(id)
        .bind(sha)
        .execute(&self.0)
        .await?
        .rows_affected();
        ensure!(n == 1, "delivery hash mismatch");
        Ok(())
    }
    pub async fn delivery_too_large(&self, id: &str, sha: &str) -> Result<()> {
        self.delivery_rejected(id, sha, "htp_report_limit").await
    }
    /// Terminal receiver rejection. `error` is one of a fixed set of codes.
    pub async fn delivery_rejected(&self, id: &str, sha: &str, error: &str) -> Result<()> {
        let n = sqlx::query("UPDATE jobs SET delivery_state='rejected',error=?,attempts=attempts+1 WHERE id=? AND report_hash=? AND delivery_state='pending'")
            .bind(error)
            .bind(id)
            .bind(sha)
            .execute(&self.0)
            .await?
            .rows_affected();
        ensure!(n == 1, "delivery state or hash mismatch");
        Ok(())
    }
    pub async fn retry(&self, id: &str, attempt: i64) -> Result<()> {
        let seconds = 30_i64
            .saturating_mul(2_i64.saturating_pow(u32::try_from(attempt.min(7))?))
            .min(3600);
        sqlx::query("UPDATE jobs SET attempts=attempts+1,next_attempt=unixepoch()+?,error='htp_delivery_failed' WHERE id=?").bind(seconds).bind(id).execute(&self.0).await?;
        Ok(())
    }
    pub async fn prune(&self) -> Result<()> {
        // Never expire pending deliveries. Keep compact idempotency tombstones indefinitely.
        sqlx::query("DELETE FROM frontier WHERE job_id IN (SELECT id FROM jobs WHERE delivery_state='delivered' AND created < unixepoch()-2592000)").execute(&self.0).await?;
        sqlx::query("UPDATE jobs SET report=NULL WHERE delivery_state='delivered' AND created < unixepoch()-2592000").execute(&self.0).await?;
        Ok(())
    }
    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.0)
            .await
            .context("database unavailable")?;
        Ok(())
    }
}

#[cfg(test)]
mod local_checkpoint_tests {
    use super::*;
    use crate::local_checkpoint;

    #[tokio::test]
    async fn robots_run_is_bounded_and_cancel_serializes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("robots.sqlite").to_str().unwrap()).await?;
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut request = report.request;
        request.request_id = uuid::Uuid::new_v4();
        let id = request.request_id.to_string();
        store.submit_local(&request).await?;
        store.claim_local(&id).await?.unwrap();
        for n in 0..34 {
            store
                .record_local_robots(&id, &format!("https://{n}.example"), "robots_txt")
                .await?;
        }
        let raw: String =
            sqlx::query_scalar("SELECT evidence FROM local_robots_run WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(value["origins"].as_array().unwrap().len(), 32);
        assert_eq!(value["omitted_origins"], 2);
        let (record, cancel) = tokio::join!(
            store.record_local_robots(&id, "https://last.example", "unavailable"),
            store.cancel(&id)
        );
        assert!(cancel?);
        let raw: String =
            sqlx::query_scalar("SELECT evidence FROM local_robots_run WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        assert_eq!(value["omitted_origins"], if record.is_ok() { 3 } else { 2 });
        assert!(
            store
                .record_local_robots(&id, "https://late.example", "unavailable")
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_and_checkpoint_serialize() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("race.sqlite").to_str().unwrap()).await?;
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut request = report.request;
        request.request_id = uuid::Uuid::new_v4();
        let id = request.request_id.to_string();
        store.submit_local(&request).await?;
        store.claim_local(&id).await?.unwrap();
        let page = crate::model::Page {
            url: request.url.to_string(),
            ..Default::default()
        };
        let evidence = local_checkpoint::capture(&request.url, &page, None);
        let (checkpoint, cancel) = tokio::join!(
            store.checkpoint_local(&id, &page, &evidence),
            store.cancel(&id)
        );
        assert!(cancel?);
        let pages = store.pages(&id).await?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM local_page_evidence WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        assert_eq!(pages.len(), usize::from(checkpoint.is_ok()));
        assert_eq!(count, i64::from(checkpoint.is_ok()));
        assert!(store.cancelled(&id).await?);
        assert!(store.checkpoint_local(&id, &page, &evidence).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn sitemap_snapshot_replay_and_cancel_are_serialized() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("sitemap.sqlite").to_str().unwrap()).await?;
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut request = report.request;
        request.request_id = uuid::Uuid::new_v4();
        let id = request.request_id.to_string();
        store.submit_local(&request).await?;
        store.claim_local(&id).await?.unwrap();
        let mut run = crate::sitemap_evidence::LocalRun::new(&[]);
        store.record_local_sitemap(&id, &run).await?;
        store.record_local_sitemap(&id, &run).await?;
        let source = url::Url::parse("https://example.com/map.xml")?;
        run.source(&source, "fetched", Some(200), Some(b"<urlset></urlset>"));
        run.state = "complete".into();
        run.discovery_state = "complete".into();
        store.record_local_sitemap(&id, &run).await?;
        store.record_local_sitemap(&id, &run).await?;
        let mut different = run.clone();
        different.sources[0].body_sha256 = Some("different".into());
        assert!(store.record_local_sitemap(&id, &different).await.is_err());
        run.state = "partial".into();
        assert!(store.record_local_sitemap(&id, &run).await.is_err());
        assert!(store.cancel(&id).await?);
        assert!(store.record_local_sitemap(&id, &run).await.is_err());
        let stored: String =
            sqlx::query_scalar("SELECT evidence FROM local_sitemap_run WHERE job_id=?")
                .bind(id)
                .fetch_one(&store.0)
                .await?;
        assert_eq!(
            serde_json::from_str::<crate::sitemap_evidence::LocalRun>(&stored)?.state,
            "complete"
        );
        Ok(())
    }

    #[tokio::test]
    async fn finalization_and_cancel_have_observable_winner() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("finish.sqlite").to_str().unwrap()).await?;
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut request = report.request;
        request.request_id = uuid::Uuid::new_v4();
        let id = request.request_id.to_string();
        store.submit_local(&request).await?;
        store.claim_local(&id).await?.unwrap();
        let (finish, cancel) = tokio::join!(store.finish_local_only(&id), store.cancel(&id));
        let cancelled = cancel?;
        assert_ne!(finish.is_ok(), cancelled);
        assert_eq!(
            store.view(&id).await?.unwrap().state,
            if cancelled { "cancelled" } else { "local_only" }
        );
        assert!(store.finish_local_only(&id).await.is_err());
        store.recover().await?;
        let origin: Option<String> =
            sqlx::query_scalar("SELECT local_finalization FROM jobs WHERE id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        assert_eq!(
            origin.as_deref(),
            if cancelled { None } else { Some("completed") }
        );
        Ok(())
    }

    #[tokio::test]
    async fn finalization_and_recovery_have_one_origin() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("race.sqlite").to_str().unwrap()).await?;
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let id = report.request.request_id.to_string();
        store.submit_local(&report.request).await?;
        store.claim_local(&id).await?.unwrap();
        let (finish, recovery) = tokio::join!(store.finish_local_only(&id), store.recover());
        recovery?;
        let origin: String = sqlx::query_scalar("SELECT local_finalization FROM jobs WHERE id=?")
            .bind(&id)
            .fetch_one(&store.0)
            .await?;
        assert_eq!(
            origin,
            if finish.is_ok() {
                "completed"
            } else {
                "interrupted"
            }
        );
        store.recover().await?;
        assert!(store.finish_local_only(&id).await.is_err());
        let after: String = sqlx::query_scalar("SELECT local_finalization FROM jobs WHERE id=?")
            .bind(&id)
            .fetch_one(&store.0)
            .await?;
        assert_eq!(after, origin);
        Ok(())
    }

    #[tokio::test]
    async fn run_budget_records_omissions_and_replays() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("budget.sqlite").to_str().unwrap()).await?;
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut request = report.request;
        request.request_id = uuid::Uuid::new_v4();
        let id = request.request_id.to_string();
        store.submit_local(&request).await?;
        store.claim_local(&id).await?.unwrap();
        sqlx::query("INSERT INTO local_page_evidence(job_id,url,evidence) VALUES(?,?,?)")
            .bind(&id)
            .bind("budget-sentinel")
            .bind("x".repeat(16 * 1024 * 1024))
            .execute(&store.0)
            .await?;
        let page = crate::model::Page {
            url: request.url.to_string(),
            ..Default::default()
        };
        let evidence = local_checkpoint::capture(&request.url, &page, None);
        store.checkpoint_local(&id, &page, &evidence).await?;
        let before: i64 =
            sqlx::query_scalar("SELECT evidence_bytes FROM local_evidence_usage WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        store.checkpoint_local(&id, &page, &evidence).await?;
        let after: i64 =
            sqlx::query_scalar("SELECT evidence_bytes FROM local_evidence_usage WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        assert_eq!(before, after);
        let actual: i64 = sqlx::query_scalar(
            "SELECT sum(length(CAST(evidence AS BLOB))) FROM local_page_evidence WHERE job_id=?",
        )
        .bind(&id)
        .fetch_one(&store.0)
        .await?;
        assert_eq!(after, actual);
        let stored: String =
            sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=? AND url=?")
                .bind(&id)
                .bind(&page.url)
                .fetch_one(&store.0)
                .await?;
        let stored: serde_json::Value = serde_json::from_str(&stored)?;
        assert_eq!(
            stored["capture"]["collectors"][0]["reason"],
            "run_byte_limit"
        );
        let next_url = request.url.join("/second")?;
        store.discover(&id, next_url.as_str(), 1, false, 10).await?;
        let next_page = crate::model::Page {
            url: next_url.to_string(),
            depth: 1,
            ..Default::default()
        };
        let next_evidence = local_checkpoint::capture(&next_url, &next_page, None);
        store
            .checkpoint_local(&id, &next_page, &next_evidence)
            .await?;
        let counter: i64 =
            sqlx::query_scalar("SELECT evidence_bytes FROM local_evidence_usage WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        let actual: i64 = sqlx::query_scalar(
            "SELECT sum(length(CAST(evidence AS BLOB))) FROM local_page_evidence WHERE job_id=?",
        )
        .bind(&id)
        .fetch_one(&store.0)
        .await?;
        assert!(counter > after);
        assert_eq!(counter, actual);
        let mut changed = evidence.clone();
        changed["version"] = 2.into();
        assert!(store.checkpoint_local(&id, &page, &changed).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn transaction_replay_conflict_rollback_and_cancel() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("test.sqlite").to_str().unwrap()).await?;
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
        let mut request = report.request;
        request.request_id = uuid::Uuid::new_v4();
        let id = request.request_id.to_string();
        store.submit_local(&request).await?;
        store.claim_local(&id).await?.unwrap();
        let page = crate::model::Page {
            url: request.url.to_string(),
            ..Default::default()
        };
        let evidence = local_checkpoint::capture(&request.url, &page, None);
        sqlx::query("CREATE TRIGGER reject_evidence BEFORE INSERT ON local_page_evidence BEGIN SELECT RAISE(ABORT, 'test'); END").execute(&store.0).await?;
        assert!(store.checkpoint_local(&id, &page, &evidence).await.is_err());
        assert!(store.pages(&id).await?.is_empty());
        let usage: i64 =
            sqlx::query_scalar("SELECT count(*) FROM local_evidence_usage WHERE job_id=?")
                .bind(&id)
                .fetch_one(&store.0)
                .await?;
        assert_eq!(usage, 0);
        sqlx::query("DROP TRIGGER reject_evidence")
            .execute(&store.0)
            .await?;
        for mismatch in [
            crate::model::Page {
                depth: 1,
                ..page.clone()
            },
            crate::model::Page {
                in_sitemap: true,
                ..page.clone()
            },
            crate::model::Page {
                is_resource: true,
                ..page.clone()
            },
        ] {
            assert!(
                store
                    .checkpoint_local(&id, &mismatch, &evidence)
                    .await
                    .is_err()
            );
        }
        assert!(store.pages(&id).await?.is_empty());
        store.checkpoint_local(&id, &page, &evidence).await?;
        store.checkpoint_local(&id, &page, &evidence).await?;
        let mut changed = evidence.clone();
        changed["url"] = "https://other.example/".into();
        assert!(store.checkpoint_local(&id, &page, &changed).await.is_err());
        let mut changed = evidence.clone();
        changed["version"] = 2.into();
        assert!(store.checkpoint_local(&id, &page, &changed).await.is_err());
        assert!(store.cancel(&id).await?);
        assert!(store.checkpoint_local(&id, &page, &evidence).await.is_err());
        assert!(store.pending().await?.is_none());
        Ok(())
    }
}
