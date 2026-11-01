use crate::model::{CrawlRequest, Report, digest};
use anyhow::{Context, Result, ensure};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};
#[derive(Clone)]
pub struct Store(pub SqlitePool);
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
        Ok(Self(pool))
    }
    pub async fn recover(&self) -> Result<()> {
        sqlx::query("UPDATE jobs SET state='queued' WHERE state='running'")
            .execute(&self.0)
            .await?;
        Ok(())
    }
    pub async fn submit(&self, r: &CrawlRequest) -> Result<bool> {
        r.validate()?;
        let id = r.request_id.to_string();
        let mut tx = self.0.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(old) = sqlx::query_scalar::<_, String>("SELECT request FROM jobs WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?
        {
            ensure!(
                serde_json::from_str::<CrawlRequest>(&old)? == *r,
                "request_id conflict"
            );
            return Ok(false);
        }
        let n:i64=sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state IN ('queued','running') OR (report IS NOT NULL AND delivery_state!='delivered')").fetch_one(&mut *tx).await?;
        ensure!(n < 100, "queue full");
        sqlx::query("INSERT INTO jobs(id,request) VALUES(?,?)")
            .bind(&id)
            .bind(serde_json::to_string(r)?)
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
        let value:Option<String>=sqlx::query_scalar("UPDATE jobs SET state='running', started_at=coalesce(started_at,strftime('%Y-%m-%dT%H:%M:%fZ','now')) WHERE id=(SELECT id FROM jobs WHERE state='queued' ORDER BY created,id LIMIT 1) RETURNING request").fetch_optional(&self.0).await?;
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
            "UPDATE jobs SET state='cancelled' WHERE id=? AND state IN ('queued','running')",
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
        let body = serde_json::to_string(r)?;
        ensure!(
            body.len() <= 16 * 1024 * 1024,
            "report exceeds delivery limit"
        );
        sqlx::query("UPDATE jobs SET report=?,report_hash=?,state=? WHERE id=?")
            .bind(&body)
            .bind(digest(body.as_bytes()))
            .bind(&r.outcome)
            .bind(r.request.request_id.to_string())
            .execute(&self.0)
            .await?;
        Ok(())
    }
    pub async fn fail(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE jobs SET state='failed',error='crawl_internal_error' WHERE id=? AND state='running'").bind(id).execute(&self.0).await?;
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
        let row=sqlx::query("SELECT id,report,report_hash,attempts FROM jobs WHERE report IS NOT NULL AND delivery_state='pending' AND next_attempt<=unixepoch() ORDER BY created LIMIT 1").fetch_optional(&self.0).await?;
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
