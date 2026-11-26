//! Read-only saved-checkpoint resource impact CLI adapter.
//!
//! This module is registered only by the binary. It performs no fetch, migration,
//! report or delivery operation and requires a quiesced immutable database snapshot.
use crate::families::JobSource;
use crate::{
    model::Page,
    resource_impact::{
        self, CollectorValue, ImpactInput, SavedPageRow, SavedResourceMeasurement,
        SavedResourceReferences,
    },
};
use anyhow::{Context, Result, ensure};
use futures_util::TryStreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::Path, str};

const MAX_ROWS: usize = 1000;
const MAX_EVIDENCE: i64 = 64 * 1024;
const MAX_PAGE: i64 = 512 * 1024;
const MAX_EVIDENCE_INPUT: usize = 2 * 1024 * 1024;
const MAX_PAGE_INPUT: usize = 8 * 1024 * 1024;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_OMISSIONS: usize = 1000;

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
    let output = analyze(&pool, id, JobSource::SavedLocal).await?;
    println!("{}", std::str::from_utf8(&output)?);
    Ok(())
}

/// The analysis behind the CLI, over any pool holding the job. `SavedLocal`
/// enforces the terminal local-only checks; `LiveStandard` reads a standard job
/// from the worker's own store.
pub async fn analyze(pool: &SqlitePool, id: &str, source: JobSource) -> Result<Vec<u8>> {
    let columns = sqlx::query("PRAGMA table_info(jobs)")
        .fetch_all(pool)
        .await?;
    let has_finalization = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "local_finalization");
    let job_query = if has_finalization {
        "SELECT CASE WHEN length(CAST(request AS BLOB)) <= 65536 THEN request END AS request,state,execution_mode,local_finalization FROM jobs WHERE id=?"
    } else {
        "SELECT CASE WHEN length(CAST(request AS BLOB)) <= 65536 THEN request END AS request,state,execution_mode,NULL AS local_finalization FROM jobs WHERE id=?"
    };
    let job = sqlx::query(job_query)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("unsupported or legacy jobs schema")?
        .context("job absent")?;
    let state: String = job.try_get("state")?;
    let mode: Option<String> = job.try_get("execution_mode")?;
    ensure!(
        source.accepts(&state, mode.as_deref()),
        "job is not terminal local-only (legacy, active, or unsupported)"
    );
    let finalization: Option<String> = job.try_get("local_finalization")?;
    let completion = source.completion(finalization.as_deref());
    let request: Option<String> = job.try_get("request")?;
    let request = request.context("saved request absent or exceeds 64 KiB")?;
    let request: Value = serde_json::from_str(&request).context("unsupported saved request")?;
    let seed = request
        .get("url")
        .and_then(Value::as_str)
        .context("saved seed absent")?;

    let mut rows = sqlx::query("SELECT CASE WHEN length(CAST(f.url AS BLOB)) <= 8192 THEN f.url END AS url, f.resource, f.page IS NOT NULL AS has_page, CASE WHEN length(CAST(f.page AS BLOB)) <= ? THEN f.page END AS page, CASE WHEN length(CAST(f.url AS BLOB)) <= 8192 AND length(CAST(e.evidence AS BLOB)) <= ? THEN e.evidence END AS evidence, e.evidence IS NOT NULL AS has_evidence FROM frontier f LEFT JOIN local_page_evidence e ON e.job_id=f.job_id AND e.url=f.url WHERE f.job_id=? ORDER BY f.resource DESC, f.url LIMIT ?")
        .bind(MAX_PAGE).bind(MAX_EVIDENCE).bind(id).bind(i64::try_from(MAX_ROWS + 1)?).fetch(pool);
    let mut saved = Vec::new();
    let mut omitted = Vec::new();
    let mut row_count = 0usize;
    let mut row_limit = false;
    let mut input_truncated = false;
    let mut evidence_input = 0usize;
    let mut page_input = 0usize;
    while let Some(row) = rows
        .try_next()
        .await
        .context("unsupported or legacy checkpoint schema")?
    {
        if row_count == MAX_ROWS {
            row_limit = true;
            input_truncated = true;
            break;
        }
        row_count += 1;
        let url: Option<String> = row.try_get("url")?;
        let Some(url) = url else {
            if omitted.len() < MAX_OMISSIONS {
                omitted.push(omission(None, "url_byte_limit"));
            }
            input_truncated = true;
            continue;
        };
        let is_resource: bool = row.try_get("resource")?;
        let has_page: bool = row.try_get("has_page")?;
        let page_raw: Option<String> = row.try_get("page")?;
        let has_evidence: bool = row.try_get("has_evidence")?;
        let evidence_raw: Option<String> = row.try_get("evidence")?;

        let mut page = None;
        let mut page_omission = None;
        if has_page {
            match page_raw {
                Some(raw) if page_input.saturating_add(raw.len()) <= MAX_PAGE_INPUT => {
                    page_input += raw.len();
                    match serde_json::from_str::<Page>(&raw) {
                        Ok(value) if value.url == url => page = Some(value),
                        _ => page_omission = Some("invalid_page_evidence"),
                    }
                }
                Some(_) => {
                    page_omission = Some("aggregate_page_input_limit");
                    input_truncated = true;
                }
                None => page_omission = Some("page_byte_limit"),
            }
        }

        if let Some(reason) = page_omission
            && omitted.len() < MAX_OMISSIONS
        {
            omitted.push(omission(Some(&url), reason));
        }
        let (resource, references) = if has_evidence {
            match evidence_raw {
                Some(raw) if evidence_input.saturating_add(raw.len()) <= MAX_EVIDENCE_INPUT => {
                    evidence_input += raw.len();
                    parse_capture(&raw, &url)
                }
                Some(_) => {
                    input_truncated = true;
                    (
                        CollectorValue::Unavailable {
                            reason: Some("aggregate_input_limit".into()),
                        },
                        CollectorValue::Unavailable {
                            reason: Some("aggregate_input_limit".into()),
                        },
                    )
                }
                None => {
                    input_truncated = true;
                    (
                        CollectorValue::Unavailable {
                            reason: Some("evidence_byte_limit".into()),
                        },
                        CollectorValue::Unavailable {
                            reason: Some("evidence_byte_limit".into()),
                        },
                    )
                }
            }
        } else {
            (CollectorValue::Absent, CollectorValue::Absent)
        };
        saved.push(SavedPageRow {
            url,
            is_resource,
            fetched: has_page,
            page,
            page_omission,
            resource,
            references,
        });
    }
    let impact = resource_impact::analyze(&ImpactInput {
        rows: &saved,
        input_truncated,
    });
    let output = serde_json::to_vec(&json!({
        "profile_version": resource_impact::PROFILE_VERSION,
        "semantics": resource_impact::SEMANTICS,
        "seed": seed,
        "terminal_state": "local_only",
        "completion": completion,
        "input_state": if saved.is_empty() { "absent" } else { "present" },
        "row_limit_reached": row_limit,
        "input_truncated": input_truncated,
        "coverage": impact.coverage,
        "bytes": impact.bytes,
        "resources": impact.resources,
        "duplicates": impact.duplicates,
        "omitted": omitted,
    }))?;
    ensure!(
        output.len() <= MAX_OUTPUT,
        "analysis output exceeds 4 MiB; no partial output produced"
    );
    Ok(output)
}

fn omission(url: Option<&str>, reason: &'static str) -> Value {
    match url {
        Some(url) => json!({
            "url": url,
            "reason": reason,
        }),
        None => json!({
            "url": null,
            "url_omitted": reason,
            "reason": reason,
        }),
    }
}

fn clipped_reason(value: Option<&str>) -> Option<String> {
    value.map(|value| value.chars().take(256).collect())
}

fn parse_capture(
    raw: &str,
    url: &str,
) -> (
    CollectorValue<SavedResourceMeasurement>,
    CollectorValue<SavedResourceReferences>,
) {
    let parsed: Result<Value, _> = serde_json::from_str(raw);
    let Ok(value) = parsed else {
        return (
            CollectorValue::Invalid {
                reason: "invalid_evidence",
            },
            CollectorValue::Invalid {
                reason: "invalid_evidence",
            },
        );
    };
    let Some(capture) = value.get("capture") else {
        return (
            CollectorValue::Invalid {
                reason: "capture_absent",
            },
            CollectorValue::Invalid {
                reason: "capture_absent",
            },
        );
    };
    if capture["version"].as_u64() != Some(1) {
        return (
            CollectorValue::Invalid {
                reason: "unsupported_capture_version",
            },
            CollectorValue::Invalid {
                reason: "unsupported_capture_version",
            },
        );
    }
    if capture.get("url").and_then(Value::as_str) != Some(url) {
        if capture.get("url") == Some(&Value::Null)
            && capture["url_omission"] == "run_byte_limit"
            && capture["url_sha256"].as_str() == Some(crate::model::digest(url.as_bytes()).as_str())
        {
            return (
                CollectorValue::Unavailable {
                    reason: Some("run_byte_limit".into()),
                },
                CollectorValue::Unavailable {
                    reason: Some("run_byte_limit".into()),
                },
            );
        }
        return (
            CollectorValue::Invalid {
                reason: "unbound_capture",
            },
            CollectorValue::Invalid {
                reason: "unbound_capture",
            },
        );
    }
    let Some(collectors) = capture["collectors"].as_array() else {
        return (
            CollectorValue::Invalid {
                reason: "invalid_collectors",
            },
            CollectorValue::Invalid {
                reason: "invalid_collectors",
            },
        );
    };
    let resource = collector(
        collectors,
        "resource",
        "duplicate_resource_collector",
        "invalid_resource_collector_data",
    );
    let references = collector(
        collectors,
        "resource_references",
        "duplicate_reference_collector",
        "invalid_reference_collector_data",
    );
    (resource, references)
}

fn collector<T: DeserializeOwned>(
    collectors: &[Value],
    name: &str,
    duplicate_reason: &'static str,
    invalid_data: &'static str,
) -> CollectorValue<T> {
    let matching: Vec<&Value> = collectors
        .iter()
        .filter(|collector| collector["collector"] == name)
        .collect();
    let Some(collector) = matching.first().copied() else {
        return CollectorValue::Absent;
    };
    if matching.len() > 1 {
        return CollectorValue::Invalid {
            reason: duplicate_reason,
        };
    }
    if collector["version"].as_u64() != Some(1) {
        return CollectorValue::Invalid {
            reason: "unsupported_collector_version",
        };
    }
    match collector["state"].as_str() {
        Some("unavailable") => CollectorValue::Unavailable {
            reason: clipped_reason(collector["reason"].as_str()),
        },
        Some("partial" | "observed") => {
            let Some(data) = collector.get("data") else {
                return CollectorValue::Invalid {
                    reason: "collector_data_absent",
                };
            };
            match serde_json::from_value::<T>(data.clone()) {
                Ok(value) if collector["state"] == "partial" => CollectorValue::Partial(value),
                Ok(value) => CollectorValue::Observed(value),
                Err(_) => CollectorValue::Invalid {
                    reason: invalid_data,
                },
            }
        }
        _ => CollectorValue::Invalid {
            reason: "unsupported_collector_state",
        },
    }
}
