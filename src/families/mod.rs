//! WP8b-2: evidence families (S02-S08) for the worker's wire-2 reports.
//!
//! `build_all` reads the job's saved capture rows through the live `Store` and returns
//! all six sections, whatever fails: an analyzer error becomes `unavailable` with
//! `analyzer_error`, an exhausted deadline `analysis_deadline`. Sections are complete
//! and sorted but not byte-budgeted (that belongs to `wire2.rs`).
pub mod common;
pub mod s02;
pub mod s03;
pub mod s04;
pub mod s05;
pub mod s06;
pub mod s08;
pub mod types;

use crate::{model::Report, store::Store};
use common::{Ctx, PageCapture, unavailable};
use futures_util::TryStreamExt;
use serde_json::Value;
use sqlx::Row;
use std::collections::HashMap;
use types::{CaptureMode, Clock, FamilyKey, FamilySection, FamilySet};

/// Reads one small fact set per page from the saved evidence rows, one row at a time.
async fn load_captures(store: &Store, id: &str) -> anyhow::Result<HashMap<String, PageCapture>> {
    let mut rows = sqlx::query("SELECT url,evidence FROM local_page_evidence WHERE job_id=?")
        .bind(id)
        .fetch(&store.0);
    let mut out = HashMap::new();
    while let Some(row) = rows.try_next().await? {
        let url: String = row.try_get("url")?;
        let evidence: String = row.try_get("evidence")?;
        let Ok(value) = serde_json::from_str::<Value>(&evidence) else {
            continue;
        };
        let mut capture = PageCapture::default();
        for c in value["capture"]["collectors"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
        {
            let Some(name) = c["collector"].as_str() else {
                continue;
            };
            let state = c["state"].as_str().unwrap_or("unavailable").to_owned();
            let reason = c["reason"].as_str().map(str::to_owned);
            match name {
                "soft_404" if c["data"].is_object() => {
                    capture.soft_404 = Some((
                        c["data"]["decision"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_owned(),
                        c["data"]["reason"].as_str().map(str::to_owned),
                    ));
                }
                "resource" => {
                    capture.body_hash = c["data"]["body_hash"].as_str().map(str::to_owned);
                }
                _ => {}
            }
            capture.collectors.insert(name.to_owned(), (state, reason));
        }
        out.insert(url, capture);
    }
    Ok(out)
}

fn semantics(key: FamilyKey) -> &'static str {
    match key {
        FamilyKey::S02 => s02::SEMANTICS,
        FamilyKey::S03 => s03::SEMANTICS,
        FamilyKey::S04 => s04::SEMANTICS,
        FamilyKey::S05 => s05::SEMANTICS,
        FamilyKey::S06 => s06::SEMANTICS,
        FamilyKey::S08 => s08::SEMANTICS,
    }
}

/// The collectors a family depends on, and which entities it applies to.
fn dependencies(key: FamilyKey) -> (&'static [&'static str], bool) {
    match key {
        FamilyKey::S02 => (&["html_occurrences", "effective_indexing"], true),
        FamilyKey::S03 | FamilyKey::S04 => (&["html_occurrences"], true),
        FamilyKey::S05 => (&["graph"], true),
        FamilyKey::S06 => (&["soft_404", "resource"], false),
        FamilyKey::S08 => (&["resource", "resource_references"], false),
    }
}

/// When every applicable page lost a dependent collector to the run budget, nothing
/// was observed: the family is `unavailable` with `run_byte_limit`.
fn all_run_limited(ctx: &Ctx<'_>, key: FamilyKey) -> bool {
    let (collectors, html_only) = dependencies(key);
    let mut any = false;
    for e in &ctx.entities {
        let applies = match key {
            FamilyKey::S08 => e.is_resource,
            _ => !html_only || e.html,
        };
        if !applies {
            continue;
        }
        any = true;
        let limited = ctx.captures.get(&e.url).is_some_and(|c| {
            collectors
                .iter()
                .any(|name| c.omission(name) == Some("run_byte_limit"))
        });
        if !limited {
            return false;
        }
    }
    any
}

pub async fn build_all(
    store: &Store,
    job_id: &str,
    report: &Report,
    capture: CaptureMode,
    clock: &dyn Clock,
) -> FamilySet {
    let sections = |reason: &str| -> FamilySet {
        FamilySet(
            FamilyKey::ALL
                .iter()
                .map(|k| (*k, unavailable(*k, semantics(*k), reason)))
                .collect(),
        )
    };
    if capture != CaptureMode::Wire2 {
        return sections("capture_mode_changed");
    }
    let Ok(captures) = load_captures(store, job_id).await else {
        return sections("analyzer_error");
    };
    let ctx = Ctx::new(report, captures, clock, capture);
    let source = JobSource::LiveStandard;
    let index = match saved_index_integrity::analyze(&store.0, job_id, source).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
        Err(_) => None,
    };
    let mut set = std::collections::BTreeMap::<FamilyKey, FamilySection>::default();
    for key in FamilyKey::ALL {
        let started = clock.now();
        if ctx.total_expired() {
            set.insert(key, unavailable(key, semantics(key), "analysis_deadline"));
            continue;
        }
        let section = match key {
            FamilyKey::S02 => index.as_ref().map(|i| s02::build(&ctx, i)),
            FamilyKey::S03 => index.as_ref().map(|i| s03::build(&ctx, i)),
            FamilyKey::S04 => Some(s04::build(&ctx, report, started)),
            FamilyKey::S05 => analyzed(saved_graph::analyze(&store.0, job_id, source).await)
                .map(|g| s05::build(&ctx, &g)),
            FamilyKey::S06 => Some(s06::build(&ctx)),
            FamilyKey::S08 => {
                analyzed(resource_impact_cli::analyze(&store.0, job_id, source).await)
                    .map(|r| s08::build(&ctx, &r))
            }
        };
        let section = match section {
            None => unavailable(key, semantics(key), "analyzer_error"),
            Some(_) if ctx.family_expired(started) => {
                unavailable(key, semantics(key), "analysis_deadline")
            }
            Some(_) if all_run_limited(&ctx, key) => {
                unavailable(key, semantics(key), "run_byte_limit")
            }
            Some(section) => section,
        };
        set.insert(key, section);
    }
    FamilySet(set)
}

fn analyzed(result: anyhow::Result<Vec<u8>>) -> Option<Value> {
    result
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

use crate::{resource_impact_cli, saved_graph, saved_index_integrity};

/// Which kind of job an analyzer entry point may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobSource {
    /// The CLI readers: a terminal local-only job in an immutable snapshot.
    SavedLocal,
    /// The worker's own standard job, read through the live store.
    LiveStandard,
}

impl JobSource {
    pub(crate) fn accepts(self, state: &str, mode: Option<&str>) -> bool {
        match self {
            JobSource::SavedLocal => state == "local_only" && mode == Some("local"),
            JobSource::LiveStandard => mode == Some("standard"),
        }
    }
    pub(crate) fn completion(self, finalization: Option<&str>) -> &'static str {
        match (self, finalization) {
            (JobSource::LiveStandard, _) => "live",
            (_, Some("completed")) => "completed",
            (_, Some("interrupted")) => "interrupted",
            _ => "unknown",
        }
    }
}
