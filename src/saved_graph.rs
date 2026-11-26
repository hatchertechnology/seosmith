use crate::families::JobSource;
use crate::{
    collection_evidence,
    graph_evidence::{self, GraphCoverageInput, LocalPageEvidence},
    integrity_evidence,
};
use anyhow::{Context, Result, ensure};
use futures_util::TryStreamExt;
use serde_json::{Value, json};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use url::Url;

const MAX_ROWS: usize = 1000;
const MAX_EVIDENCE: i64 = 64 * 1024;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_INPUT: usize = 2 * 1024 * 1024;
const MAX_LINKS: usize = 10_000;
const MAX_ESTIMATED_OUTPUT: usize = 3 * 1024 * 1024;

fn collector_collection(
    collectors: &[Value],
    name: &str,
) -> Option<collection_evidence::Collection> {
    let collector = collectors.iter().find(|c| c["collector"] == name)?;
    if collector["version"].as_u64() != Some(1)
        || !matches!(collector["state"].as_str(), Some("observed" | "partial"))
    {
        return None;
    }
    serde_json::from_value(collector["data"].clone()).ok()
}

/// Page-level followability from the crawler's own retained directive evaluation.
/// Unknown or absent evidence stays `None` rather than assuming permission.
fn page_follow(collectors: &[Value]) -> Option<bool> {
    let collector = collectors
        .iter()
        .find(|c| c["collector"] == "effective_indexing")?;
    if collector["version"].as_u64() != Some(1)
        || !matches!(collector["state"].as_str(), Some("observed" | "partial"))
    {
        return None;
    }
    let decisions = collector["data"]["decisions"].as_array()?;
    let decision = decisions.iter().find(|d| {
        d["bot"]
            .as_str()
            .is_some_and(|bot| bot.eq_ignore_ascii_case("Seosmith"))
    })?;
    match decision["directives"]["follow"].as_str()? {
        "Allowed" => Some(true),
        "Restricted" => Some(false),
        _ => None,
    }
}

/// A conclusive retained canonical declaration. `false` means the retained
/// evidence cannot determine a single declaration, so absence of a variant on
/// that page must not be promoted to a negative result. A page that names
/// itself is still a declaration: it counts with the retained source and
/// version, while `saved_canonical_variant_links` excludes the self-reference.
fn canonical_declaration(
    collectors: &[Value],
    url: &str,
    scope: Option<&Url>,
) -> (bool, Option<graph_evidence::CanonicalDeclaration>) {
    let (Some(html), Some(http), Some(scope)) = (
        collector_collection(collectors, "html_occurrences"),
        collector_collection(collectors, "http_occurrences"),
        scope,
    ) else {
        return (false, None);
    };
    let Some(state) = collectors
        .iter()
        .find(|c| c["collector"] == "effective_indexing")
        .and_then(|c| c["data"]["state"].as_str())
    else {
        return (false, None);
    };
    let Ok(base) = Url::parse(url) else {
        return (false, None);
    };
    let capture = collection_evidence::PageCapture {
        version: "s02-s03-local-1".into(),
        url: url.into(),
        state: state.into(),
        robots: vec![],
        robots_source: String::new(),
        http: Some(http),
        html: Some(html),
    };
    let assessment = integrity_evidence::canonical_assessment(&capture, &base, scope);
    match assessment.agreement {
        integrity_evidence::CanonicalAgreement::Consistent => {
            match assessment
                .declarations
                .iter()
                .find(|d| {
                    d.state == integrity_evidence::DeclarationState::Valid && d.target.is_some()
                })
                .and_then(|d| d.target.clone().map(|target| (d.source.clone(), target)))
            {
                Some((source, target)) => (
                    true,
                    Some(graph_evidence::CanonicalDeclaration {
                        page: url.into(),
                        declared_canonical: target,
                        declaration_source: source,
                        declaration_version: 1,
                    }),
                ),
                None => (false, None),
            }
        }
        integrity_evidence::CanonicalAgreement::Absent => (true, None),
        integrity_evidence::CanonicalAgreement::Conflicting
        | integrity_evidence::CanonicalAgreement::Unknown => (false, None),
    }
}

/// Retained page evidence kept parallel to the retained graph pages.
struct RetainedMeta {
    page_follow: Option<bool>,
    sitemap_listed: Option<bool>,
    canonical_conclusive: bool,
    declaration: Option<graph_evidence::CanonicalDeclaration>,
}

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
    let scope = Url::parse(seed).ok();
    let mut rows = sqlx::query("SELECT CASE WHEN length(CAST(f.url AS BLOB)) <= 8192 THEN f.url END AS url, f.page IS NOT NULL AS has_page, f.in_sitemap AS in_sitemap, CASE WHEN length(CAST(f.url AS BLOB)) <= 8192 AND length(CAST(e.evidence AS BLOB)) <= ? THEN e.evidence END AS evidence, e.evidence IS NOT NULL AS has_evidence FROM frontier f LEFT JOIN local_page_evidence e ON e.job_id=f.job_id AND e.url=f.url WHERE f.job_id=? ORDER BY f.url LIMIT ?")
        .bind(MAX_EVIDENCE).bind(id).bind(i64::try_from(MAX_ROWS + 1)?).fetch(pool);
    let mut row_count = 0usize;
    let mut row_limit = false;
    let mut pages = Vec::new();
    let mut metas: Vec<RetainedMeta> = Vec::new();
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
        let in_sitemap: bool = row.try_get("in_sitemap")?;
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
        if capture["version"].as_u64() == Some(1)
            && capture.get("url") == Some(&Value::Null)
            && capture["url_omission"] == "run_byte_limit"
            && capture["url_sha256"].as_str() == Some(&crate::model::digest(url.as_bytes()))
        {
            omitted.push(json!({"url":url,"reason":"run_byte_limit"}));
            continue;
        }
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
        let collectors = collectors.map_or(&[][..], Vec::as_slice);
        let (canonical_conclusive, declaration) =
            canonical_declaration(collectors, &url, scope.as_ref());
        metas.push(RetainedMeta {
            page_follow: page_follow(collectors),
            sitemap_listed: in_sitemap.then_some(true),
            canonical_conclusive,
            declaration,
        });
        pages.push(item);
    }
    // A sitemap/declaration relationship is retained evidence, not proof of first
    // discovery or of navigation. A negative canonical result also requires that
    // declaration coverage is observed and complete: an omitted page or a
    // truncated row set could have declared a canonical that is invisible here,
    // so those scopes stay unknown rather than being promoted to `not_declared`.
    let any_unknown_canonical = metas.iter().any(|meta| !meta.canonical_conclusive);
    let declaration_coverage_complete = omitted.is_empty() && !row_limit;
    let declared_targets: BTreeSet<&str> = metas
        .iter()
        .filter_map(|meta| {
            meta.declaration
                .as_ref()
                .map(|declaration| declaration.declared_canonical.as_str())
        })
        .collect();
    let mut evidence = BTreeMap::new();
    for (page, meta) in pages.iter().zip(&metas) {
        let canonical_target = if declared_targets.contains(page.url.as_str()) {
            Some(true)
        } else if any_unknown_canonical || !declaration_coverage_complete {
            None
        } else {
            Some(false)
        };
        evidence.insert(
            page.url.clone(),
            graph_evidence::PageEvidence {
                page_follow: meta.page_follow,
                sitemap_listed: meta.sitemap_listed,
                canonical_target,
            },
        );
    }
    let declarations: Vec<_> = metas
        .iter()
        .filter_map(|meta| meta.declaration.clone())
        .collect();
    let canonical_variants = graph_evidence::saved_canonical_variant_links(&pages, &declarations)
        .map_err(|_| anyhow::anyhow!("duplicate page URLs"))?;
    let coverage = GraphCoverageInput {
        discovery_complete: None,
        expected_pages: if row_limit { None } else { Some(row_count) },
        observed_pages: pages.len(),
        unavailable_pages: omitted.len(),
        truncated_pages: pages.iter().filter(|p| !p.extraction_complete).count(),
        omitted_records: if row_limit { None } else { Some(omitted.len()) },
        comparisons_complete: Some(true),
    };
    let nodes = graph_evidence::graph_with_evidence(seed, &pages, &coverage, &evidence)
        .map_err(|_| anyhow::anyhow!("duplicate page URLs"))?;
    let fragments = graph_evidence::fragment_checks(&pages)
        .map_err(|_| anyhow::anyhow!("duplicate page URLs"))?;
    ensure!(
        estimated_output.saturating_add(serde_json::to_vec(&canonical_variants)?.len())
            <= MAX_ESTIMATED_OUTPUT,
        "analysis output exceeds 3 MiB graph-result estimate; no partial output produced"
    );
    let output = serde_json::to_vec(
        &json!({"semantics":"seosmith-saved-graph-2","seed":seed,"terminal_state":"local_only", "input_state":if row_count == 0 {"absent"} else {"present"}, "completion":completion, "coverage":graph_evidence::graph_coverage(&coverage), "row_limit_reached":row_limit, "omitted":omitted, "nodes":nodes, "fragments":fragments, "canonical_variants":canonical_variants}),
    )?;
    ensure!(
        output.len() <= MAX_OUTPUT,
        "analysis output exceeds 4 MiB; no partial output produced"
    );
    Ok(output)
}
