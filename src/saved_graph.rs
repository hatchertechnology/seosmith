use anyhow::{Context, Result, ensure};
use futures_util::TryStreamExt;
use seosmith::graph_evidence::{self, GraphCoverageInput, LocalPageEvidence};
use serde_json::{Value, json};
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{collections::BTreeSet, path::Path};

const MAX_ROWS: usize = 1000;
const MAX_EVIDENCE: i64 = 64 * 1024;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_INPUT: usize = 2 * 1024 * 1024;
const MAX_LINKS: usize = 10_000;
const MAX_ESTIMATED_OUTPUT: usize = 3 * 1024 * 1024;

pub async fn run(path: &Path, id: &str) -> Result<()> {
    ensure!(path.is_file(), "database does not exist");
    // Immutable mode never creates SQLite sidecars. Refuse WAL even if empty:
    // ignoring a live or uncheckpointed WAL could silently analyze stale data.
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    ensure!(
        !Path::new(&wal).exists(),
        "database has a WAL; close writers and checkpoint it before analysis"
    );
    let mut journal = path.as_os_str().to_os_string();
    journal.push("-journal");
    ensure!(
        !Path::new(&journal).exists(),
        "database has a rollback journal; use a quiesced snapshot"
    );
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .context("open existing database read-only")?;
    let job = sqlx::query("SELECT CASE WHEN length(CAST(request AS BLOB)) <= 65536 THEN request END AS request,state,execution_mode FROM jobs WHERE id=?")
        .bind(id)
        .fetch_optional(&pool)
        .await
        .context("unsupported or legacy jobs schema")?
        .context("job absent")?;
    let state: String = job.try_get("state")?;
    let mode: Option<String> = job.try_get("execution_mode")?;
    ensure!(
        state == "local_only" && mode.as_deref() == Some("local"),
        "job is not terminal local-only (legacy, active, or unsupported)"
    );
    let request: Option<String> = job.try_get("request")?;
    let request = request.context("saved request absent or exceeds 64 KiB")?;
    let request: Value = serde_json::from_str(&request).context("unsupported saved request")?;
    let seed = request
        .get("url")
        .and_then(Value::as_str)
        .context("saved seed absent")?;
    let mut rows = sqlx::query("SELECT CASE WHEN length(CAST(f.url AS BLOB)) <= 8192 THEN f.url END AS url, f.page IS NOT NULL AS has_page, CASE WHEN length(CAST(f.url AS BLOB)) <= 8192 AND length(CAST(e.evidence AS BLOB)) <= ? THEN e.evidence END AS evidence, e.evidence IS NOT NULL AS has_evidence FROM frontier f LEFT JOIN local_page_evidence e ON e.job_id=f.job_id AND e.url=f.url WHERE f.job_id=? ORDER BY f.url LIMIT ?")
        .bind(MAX_EVIDENCE).bind(id).bind(i64::try_from(MAX_ROWS + 1)?).fetch(&pool);
    let mut row_count = 0usize;
    let mut row_limit = false;
    let mut pages = Vec::new();
    let mut omitted = Vec::new();
    let mut unique_urls = BTreeSet::new();
    let mut input_bytes = 0usize;
    let mut links = 0usize;
    let mut estimated_output = 0usize;
    while let Some(row) = rows
        .try_next()
        .await
        .context("unsupported or legacy checkpoint schema")?
    {
        if row_count == MAX_ROWS {
            row_limit = true;
            break;
        }
        row_count += 1;
        let url: Option<String> = row.try_get("url")?;
        let Some(url) = url else {
            omitted.push(json!({"url":null,"reason":"url_byte_limit"}));
            continue;
        };
        let page: bool = row.try_get("has_page")?;
        let present: bool = row.try_get("has_evidence")?;
        let evidence: Option<String> = row.try_get("evidence")?;
        let reason = if !page {
            Some("page_checkpoint_absent")
        } else if !present {
            Some("graph_evidence_absent_or_legacy")
        } else if evidence.is_none() {
            Some("evidence_byte_limit")
        } else {
            None
        };
        if let Some(reason) = reason {
            omitted.push(json!({"url": url, "reason": reason}));
            continue;
        }
        let raw = evidence.as_deref().unwrap_or_default();
        if input_bytes.saturating_add(raw.len()) > MAX_INPUT {
            omitted.push(json!({"url":url,"reason":"aggregate_input_limit"}));
            continue;
        }
        input_bytes += raw.len();
        let parsed: Result<Value, _> = serde_json::from_str(raw);
        let Ok(value) = parsed else {
            omitted.push(json!({"url":url,"reason":"invalid_evidence"}));
            continue;
        };
        let capture = &value["capture"];
        let collectors = capture["collectors"].as_array();
        if capture["version"].as_u64() != Some(1)
            || capture["url"].as_str() != Some(&url)
            || collectors.is_none()
        {
            omitted.push(json!({"url":url,"reason":"unsupported_or_unbound_capture"}));
            continue;
        }
        let graph: Vec<_> = collectors
            .into_iter()
            .flatten()
            .filter(|c| c["collector"] == "graph")
            .collect();
        if graph.len() != 1 {
            omitted.push(json!({"url":url,"reason":if graph.is_empty() {"graph_collector_absent"} else {"duplicate_graph_collector"}}));
            continue;
        }
        let g = graph[0];
        if g["version"].as_u64() != Some(1)
            || !matches!(g["state"].as_str(), Some("observed" | "partial"))
        {
            omitted.push(json!({"url":url,"reason":"graph_unavailable_or_unsupported","collector_reason":g["reason"].as_str().filter(|r| r.len() <= 256).unwrap_or("unspecified_or_oversized")}));
            continue;
        }
        let Ok(mut item) = serde_json::from_value::<LocalPageEvidence>(g["data"].clone()) else {
            omitted.push(json!({"url":url,"reason":"invalid_graph_data"}));
            continue;
        };
        if item.url != url || !unique_urls.insert(item.url.clone()) {
            omitted.push(json!({"url":url,"reason":"duplicate_or_unbound_graph_url"}));
            continue;
        }
        // JSON escaping can expand a byte to six bytes. Bound all node and
        // fragment strings before graph/fragment output vectors are built.
        let estimate = item.links.iter().fold(
            item.url.len().saturating_mul(6).saturating_add(512),
            |sum, link| {
                if let Some(fragment) = &link.fragment {
                    sum.saturating_add(
                        item.url
                            .len()
                            .saturating_add(link.fetch_url.len())
                            .saturating_add(fragment.len())
                            .saturating_mul(6)
                            .saturating_add(256),
                    )
                } else {
                    sum
                }
            },
        );
        if links.saturating_add(item.links.len()) > MAX_LINKS
            || estimated_output.saturating_add(estimate) > MAX_ESTIMATED_OUTPUT
        {
            omitted.push(json!({"url":url,"reason":"analysis_budget"}));
            continue;
        }
        links += item.links.len();
        estimated_output += estimate;
        if g["state"] == "partial" {
            item.extraction_complete = false;
        }
        pages.push(item);
    }
    let coverage = GraphCoverageInput {
        discovery_complete: None,
        expected_pages: if row_limit { None } else { Some(row_count) },
        observed_pages: pages.len(),
        unavailable_pages: omitted.len(),
        truncated_pages: pages.iter().filter(|p| !p.extraction_complete).count(),
        omitted_records: if row_limit { None } else { Some(omitted.len()) },
        comparisons_complete: Some(true),
    };
    let nodes = graph_evidence::graph(seed, &pages, &coverage)
        .map_err(|_| anyhow::anyhow!("duplicate page URLs"))?;
    let fragments = graph_evidence::fragment_checks(&pages)
        .map_err(|_| anyhow::anyhow!("duplicate page URLs"))?;
    let output = serde_json::to_vec(
        &json!({"seed":seed,"terminal_state":"local_only", "input_state":if row_count == 0 {"absent"} else {"present"}, "completion":"unknown: recovery also closes interrupted jobs", "coverage":graph_evidence::graph_coverage(&coverage), "row_limit_reached":row_limit, "omitted":omitted, "nodes":nodes, "fragments":fragments}),
    )?;
    ensure!(
        output.len() <= MAX_OUTPUT,
        "analysis output exceeds 4 MiB; no partial output produced"
    );
    println!("{}", std::str::from_utf8(&output)?);
    Ok(())
}
