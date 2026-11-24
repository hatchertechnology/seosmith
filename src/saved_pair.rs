//! Read-only S06 pair comparison over saved local checkpoints or retained HTML.
//!
//! Per-page soft-404 evidence is read from `local_page_evidence`; the saved
//! evidence does not retain main text, so cross-page similarity stays unknown
//! unless the source bytes are identical. Retained HTML is supplied by the
//! caller and is never fetched or persisted.
use anyhow::{Context, Result, ensure};
use seosmith::s06_detector::{self, Decision, Evidence, Side as DetectorSide, Source};
use serde_json::{Value, json};
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{fs::File, io::Read, path::Path};
use url::Url;

const MAX_URL_BYTES: usize = 8192;
const MAX_EVIDENCE_BYTES: i64 = 64 * 1024;
const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_REASON_CHARS: usize = 256;
const MAX_EVIDENCE_TEXT_CHARS: usize = 32 * 1024;
const MAX_EXCERPT_CHARS: usize = 240;

pub struct SavedPairArgs<'a> {
    pub request_id: Option<uuid::Uuid>,
    pub source: Option<&'a str>,
    pub peer: Option<&'a str>,
    pub source_html: Option<&'a Path>,
    pub peer_html: Option<&'a Path>,
    pub source_status: Option<u16>,
    pub peer_status: Option<u16>,
    pub source_content_type: Option<&'a str>,
    pub peer_content_type: Option<&'a str>,
}

pub async fn run(database: &Path, args: SavedPairArgs<'_>) -> Result<()> {
    let value = match (
        args.source_html,
        args.peer_html,
        args.request_id,
        args.source,
        args.peer,
    ) {
        (Some(source), Some(peer), None, None, None) => {
            ensure!(
                args.source_status.is_some() && args.peer_status.is_some(),
                "retained HTML mode requires --source-status and --peer-status supplied by the caller"
            );
            let source_status = args.source_status.context("source status required")?;
            let peer_status = args.peer_status.context("peer status required")?;
            compare_html(
                source,
                peer,
                source_status,
                peer_status,
                args.source_content_type.unwrap_or("text/html"),
                args.peer_content_type.unwrap_or("text/html"),
            )?
        }
        (None, None, Some(request_id), Some(source), Some(peer)) => {
            ensure!(
                args.source_status.is_none()
                    && args.peer_status.is_none()
                    && args.source_content_type.is_none()
                    && args.peer_content_type.is_none(),
                "saved checkpoint mode does not accept retained-response metadata flags"
            );
            compare_saved(database, request_id, source, peer).await?
        }
        _ => anyhow::bail!(
            "use --request-id with --source and --peer, or --source-html with --peer-html"
        ),
    };
    let output = serde_json::to_vec(&value)?;
    ensure!(
        output.len() <= MAX_OUTPUT_BYTES,
        "comparison output exceeds 256 KiB"
    );
    println!("{}", std::str::from_utf8(&output)?);
    Ok(())
}

async fn open_snapshot(path: &Path) -> Result<sqlx::SqlitePool> {
    ensure!(path.is_file(), "database does not exist");
    // Immutable mode never creates SQLite sidecars. Refuse WAL even if empty:
    // ignoring a live or uncheckpointed WAL could silently compare stale data.
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    ensure!(
        !Path::new(&wal).exists(),
        "database has a WAL; close writers and checkpoint it before comparison"
    );
    let mut journal = path.as_os_str().to_os_string();
    journal.push("-journal");
    ensure!(
        !Path::new(&journal).exists(),
        "database has a rollback journal; use a quiesced snapshot"
    );
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .read_only(true)
                .immutable(true),
        )
        .await
        .context("open existing database read-only")
}

fn validate_url(value: &str) -> Result<()> {
    ensure!(value.len() <= MAX_URL_BYTES, "URL exceeds 8192 bytes");
    Url::parse(value).context("saved page URL is not absolute")?;
    Ok(())
}

async fn compare_saved(
    database: &Path,
    request_id: uuid::Uuid,
    source: &str,
    peer: &str,
) -> Result<Value> {
    validate_url(source)?;
    validate_url(peer)?;
    ensure!(
        source != peer,
        "source and peer URLs must be distinct pages"
    );
    let id = request_id.to_string();
    let pool = open_snapshot(database).await?;
    let columns = sqlx::query("PRAGMA table_info(jobs)")
        .fetch_all(&pool)
        .await?;
    let has_finalization = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "local_finalization");
    let job_query = if has_finalization {
        "SELECT state,execution_mode,local_finalization FROM jobs WHERE id=?"
    } else {
        "SELECT state,execution_mode,NULL AS local_finalization FROM jobs WHERE id=?"
    };
    let job = sqlx::query(job_query)
        .bind(&id)
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
    let finalization: Option<String> = job.try_get("local_finalization")?;
    let completion = match finalization.as_deref() {
        Some("completed") => "completed",
        Some("interrupted") => "interrupted",
        _ => "unknown",
    };
    let source_side = load_side(&pool, &id, source).await;
    let peer_side = load_side(&pool, &id, peer).await;
    let comparison = compare_saved_sides(&source_side, &peer_side);
    Ok(json!({
        "version": 1,
        "mode": "saved_checkpoints",
        "semantics": s06_detector::SEMANTICS,
        "provenance": {
            "job_id": id,
            "job_state": state,
            "execution_mode": "local",
            "completion": completion,
            "evidence_source": "local_page_evidence.soft_404",
            "database_access": "read_only_immutable",
            "similarity_inputs": "main_text_not_retained",
            "network_access": false,
            "report_or_delivery_write": false
        },
        "source": source_side.to_json(),
        "peer": peer_side.to_json(),
        "comparison": comparison
    }))
}

async fn load_side(pool: &sqlx::SqlitePool, id: &str, url: &str) -> SavedSide {
    let query = "SELECT f.page IS NOT NULL AS has_page, f.page AS page, CASE WHEN length(CAST(e.evidence AS BLOB)) <= ? THEN e.evidence END AS evidence, e.evidence IS NOT NULL AS has_evidence FROM frontier f LEFT JOIN local_page_evidence e ON e.job_id=f.job_id AND e.url=f.url WHERE f.job_id=? AND f.url=?";
    let Ok(row) = sqlx::query(query)
        .bind(MAX_EVIDENCE_BYTES)
        .bind(id)
        .bind(url)
        .fetch_optional(pool)
        .await
    else {
        return SavedSide::unavailable(url, "unsupported_or_legacy_checkpoint_schema");
    };
    let Some(row) = row else {
        return SavedSide::unavailable(url, "page_not_in_saved_run");
    };
    let has_page = row.try_get::<bool, _>("has_page").unwrap_or(false);
    let has_evidence = row.try_get::<bool, _>("has_evidence").unwrap_or(false);
    if !has_page {
        return SavedSide::unavailable(url, "page_checkpoint_absent");
    }
    if !has_evidence {
        return SavedSide::unavailable(url, "page_evidence_absent");
    }
    let evidence: Option<String> = row.try_get("evidence").unwrap_or(None);
    let Some(raw) = evidence else {
        return SavedSide::unavailable(url, "evidence_byte_limit");
    };
    // The frontier page JSON retains the crawled status; byte equality is only
    // meaningful for complete 200 responses, matching the detector's gate.
    let page: Option<String> = row.try_get("page").unwrap_or(None);
    let http_status = page
        .as_deref()
        .and_then(|page| serde_json::from_str::<Value>(page).ok())
        .and_then(|page| page.get("status").and_then(Value::as_u64))
        .and_then(|status| u16::try_from(status).ok());
    SavedSide::parse(url, http_status, &raw)
}

#[derive(Debug)]
struct EvidenceSummary {
    source_sha256: String,
    main_sha256: String,
    title: String,
    h1: String,
    excerpt: String,
    template_fraction: f64,
}

impl From<&Evidence> for EvidenceSummary {
    fn from(evidence: &Evidence) -> Self {
        Self {
            source_sha256: evidence.source_sha256.clone(),
            main_sha256: evidence.main_sha256.clone(),
            title: evidence.title.clone(),
            h1: evidence.h1.clone(),
            excerpt: evidence.excerpt.clone(),
            template_fraction: evidence.template_fraction,
        }
    }
}

#[derive(Debug)]
struct SavedSide {
    url: String,
    http_status: Option<u16>,
    checkpoint_state: String,
    checkpoint_reason: Option<String>,
    assessment: Option<Value>,
    evidence: Option<EvidenceSummary>,
    unusable: Option<String>,
}

impl SavedSide {
    fn unavailable(url: &str, reason: &str) -> Self {
        Self {
            url: url.to_owned(),
            http_status: None,
            checkpoint_state: "unavailable".into(),
            checkpoint_reason: Some(reason.to_owned()),
            assessment: None,
            evidence: None,
            unusable: Some(reason.to_owned()),
        }
    }

    fn parse(url: &str, http_status: Option<u16>, raw: &str) -> Self {
        let value: Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) => return Self::unavailable(url, "invalid_evidence"),
        };
        let Some(capture) = value.get("capture") else {
            return Self::unavailable(url, "unsupported_evidence_envelope");
        };
        let Some(input_hash) = value.get("input_hash").and_then(Value::as_str) else {
            return Self::unavailable(url, "unsupported_evidence_envelope");
        };
        // The run-budget marker reuses the pre-replacement input digest; bind it
        // by URL digest before checking the envelope digest.
        if capture.get("url") == Some(&Value::Null)
            && capture.get("url_omission").and_then(Value::as_str) == Some("run_byte_limit")
        {
            return if capture.get("url_sha256").and_then(Value::as_str)
                == Some(seosmith::model::digest(url.as_bytes()).as_str())
            {
                Self::unavailable(url, "run_byte_limit")
            } else {
                Self::unavailable(url, "unsupported_or_unbound_capture")
            };
        }
        let Ok(canonical) = serde_json::to_vec(capture) else {
            return Self::unavailable(url, "invalid_evidence");
        };
        if seosmith::model::digest(&canonical) != input_hash {
            return Self::unavailable(url, "evidence_integrity_mismatch");
        }
        if capture.get("version").and_then(Value::as_u64) != Some(1)
            || capture.get("url").and_then(Value::as_str) != Some(url)
        {
            return Self::unavailable(url, "unsupported_or_unbound_capture");
        }
        let Some(collectors) = capture.get("collectors").and_then(Value::as_array) else {
            return Self::unavailable(url, "invalid_capture");
        };
        let soft: Vec<_> = collectors
            .iter()
            .filter(|collector| {
                collector.get("collector").and_then(Value::as_str) == Some("soft_404")
            })
            .collect();
        let [collector] = soft.as_slice() else {
            return Self::unavailable(
                url,
                if soft.is_empty() {
                    "soft_404_collector_absent"
                } else {
                    "duplicate_soft_404_collector"
                },
            );
        };
        if collector.get("version").and_then(Value::as_u64) != Some(1) {
            return Self::unavailable(url, "unsupported_soft_404_collector_version");
        }
        let Some(state) = collector.get("state").and_then(Value::as_str) else {
            return Self::unavailable(url, "invalid_soft_404_collector_state");
        };
        if !matches!(state, "observed" | "partial" | "unavailable") {
            return Self::unavailable(url, "invalid_soft_404_collector_state");
        }
        let collector_reason = bounded_reason(collector.get("reason"));
        if state == "unavailable" {
            let reason = collector_reason.unwrap_or_else(|| "assessment_unavailable".into());
            let mut side = Self::unavailable(url, &reason);
            side.checkpoint_state = "unavailable".into();
            return side;
        }
        let Some(data) = collector.get("data").and_then(Value::as_object) else {
            return Self::unavailable(url, "assessment_data_absent");
        };
        let Some(semantics) = data.get("semantics").and_then(Value::as_str) else {
            return Self::unavailable(url, "assessment_semantics_absent");
        };
        let Some(decision) = data.get("decision").and_then(Value::as_str) else {
            return Self::unavailable(url, "assessment_decision_absent");
        };
        if !matches!(decision, "candidate" | "no_candidate" | "unknown") {
            return Self::unavailable(url, "invalid_assessment_decision");
        }
        let reason = data
            .get("reason")
            .and_then(Value::as_str)
            .map_or_else(|| "unspecified".into(), bounded_reason_text);
        let evidence = data.get("evidence").and_then(parse_evidence);
        let assessment = json!({
            "semantics": semantics,
            "decision": decision,
            "reason": reason,
            "evidence": evidence.as_ref().map(evidence_json)
        });
        let unusable = if semantics != s06_detector::SEMANTICS {
            Some("incompatible_saved_semantics".to_owned())
        } else if evidence.is_none() {
            Some("invalid_or_absent_assessment_evidence".to_owned())
        } else if http_status != Some(200) {
            // The detector only permits exact-byte candidates after both sides
            // pass the complete HTTP 200 and available-main gates. A saved page
            // whose frontier status is not 200 (for example a 404/410 error
            // page) must never become a byte-equality candidate.
            Some("saved_side_not_http_200".to_owned())
        } else if decision == "unknown" {
            Some(reason.clone())
        } else {
            None
        };
        Self {
            url: url.to_owned(),
            http_status,
            checkpoint_state: state.into(),
            checkpoint_reason: if state == "observed" {
                None
            } else {
                collector_reason.or(Some(reason.clone()))
            },
            assessment: Some(assessment),
            evidence,
            unusable,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "input": "saved_checkpoint",
            "url": self.url,
            "url_sha256": seosmith::model::digest(self.url.as_bytes()),
            "http_status": self.http_status,
            "checkpoint": {
                "state": self.checkpoint_state,
                "reason": self.checkpoint_reason
            },
            "assessment": self.assessment
        })
    }
}

fn bounded_reason_text(reason: &str) -> String {
    if reason.chars().count() <= MAX_REASON_CHARS {
        reason.to_owned()
    } else {
        "unspecified_or_oversized".into()
    }
}

fn bounded_reason(value: Option<&Value>) -> Option<String> {
    Some(bounded_reason_text(value?.as_str()?))
}

fn bounded_text(value: Option<&Value>, limit: usize) -> Option<String> {
    let text = value?.as_str()?;
    (text.chars().count() <= limit).then(|| text.to_owned())
}

fn parse_evidence(value: &Value) -> Option<EvidenceSummary> {
    let source_sha256 = bounded_text(value.get("source_sha256"), 64)?;
    let main_sha256 = bounded_text(value.get("main_sha256"), 64)?;
    if source_sha256.len() != 64
        || main_sha256.len() != 64
        || !source_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !main_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let title = bounded_text(value.get("title"), MAX_EVIDENCE_TEXT_CHARS)?;
    let h1 = bounded_text(value.get("h1"), MAX_EVIDENCE_TEXT_CHARS)?;
    let excerpt = bounded_text(value.get("excerpt"), MAX_EXCERPT_CHARS)?;
    let template_fraction = value.get("template_fraction")?.as_f64()?;
    if !template_fraction.is_finite() || !(0.0..=1.0).contains(&template_fraction) {
        return None;
    }
    Some(EvidenceSummary {
        source_sha256,
        main_sha256,
        title,
        h1,
        excerpt,
        template_fraction,
    })
}

fn evidence_json(evidence: &EvidenceSummary) -> Value {
    json!({
        "source_sha256": evidence.source_sha256,
        "main_sha256": evidence.main_sha256,
        "title": evidence.title,
        "h1": evidence.h1,
        "excerpt": evidence.excerpt,
        "template_fraction": evidence.template_fraction
    })
}

fn compare_saved_sides(source: &SavedSide, peer: &SavedSide) -> Value {
    if let Some(reason) = &source.unusable {
        let mut omissions = vec![(Some("source"), reason.clone())];
        if let Some(peer_reason) = &peer.unusable {
            omissions.push((Some("peer"), peer_reason.clone()));
        }
        return comparison(
            "unknown",
            reason,
            "source_assessment_unavailable",
            Some("source"),
            omissions,
        );
    }
    if let Some(reason) = &peer.unusable {
        return comparison(
            "unknown",
            reason,
            "peer_assessment_unavailable",
            Some("peer"),
            vec![(Some("peer"), reason.clone())],
        );
    }
    let (Some(a), Some(b)) = (&source.evidence, &peer.evidence) else {
        return comparison(
            "unknown",
            "comparison evidence unavailable",
            "saved_evidence_insufficient",
            None,
            vec![(Some("source_or_peer"), "evidence_absent".into())],
        );
    };
    let mut value = if a.source_sha256 == b.source_sha256 {
        comparison(
            "candidate",
            "identical source bytes",
            "identical_source_bytes",
            None,
            vec![(None, "similarity_score_not_retained".into())],
        )
    } else {
        comparison(
            "unknown",
            "saved evidence lacks main-text tokens for similarity",
            "saved_evidence_insufficient",
            None,
            vec![
                (None, "main_text_tokens_not_retained".into()),
                (None, "similarity_score_not_retained".into()),
            ],
        )
    };
    value["observations"] = json!({
        "source_bytes_equal": a.source_sha256 == b.source_sha256,
        "main_text_hash_equal": a.main_sha256 == b.main_sha256,
        "notes": "equal main-text hashes alone do not establish detector similarity support or SKU suppression"
    });
    value
}

fn comparison(
    decision: &str,
    reason: &str,
    basis: &str,
    unavailable_side: Option<&str>,
    omissions: Vec<(Option<&str>, String)>,
) -> Value {
    json!({
        "decision": decision,
        "reason": reason,
        "basis": basis,
        "similarity": Value::Null,
        "unavailable_side": unavailable_side,
        "omissions": omissions
            .into_iter()
            .map(|(side, reason)| json!({"side": side, "reason": reason}))
            .collect::<Vec<_>>()
    })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?
        .take(u64::try_from(MAX_SOURCE_BYTES + 1)?)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn decision_name(decision: Decision) -> &'static str {
    match decision {
        Decision::Candidate => "candidate",
        Decision::NoCandidate => "no_candidate",
        Decision::Unknown => "unknown",
    }
}

fn detector_side(side: Option<DetectorSide>) -> Option<&'static str> {
    match side {
        Some(DetectorSide::Source) => Some("source"),
        Some(DetectorSide::Peer) => Some("peer"),
        None => None,
    }
}

fn compare_html(
    source: &Path,
    peer: &Path,
    source_status: u16,
    peer_status: u16,
    source_content_type: &str,
    peer_content_type: &str,
) -> Result<Value> {
    let source_bytes = read_bounded(source)?;
    let peer_bytes = read_bounded(peer)?;
    let source_input = Source {
        bytes: Some(&source_bytes),
        status: Some(source_status),
        content_type: source_content_type,
        complete: true,
    };
    let peer_input = Source {
        bytes: Some(&peer_bytes),
        status: Some(peer_status),
        content_type: peer_content_type,
        complete: true,
    };
    let out = s06_detector::compare(&source_input, Some(&peer_input));
    let source_extraction = extraction_json(
        &source_bytes,
        out.evidence.as_ref(),
        out.unavailable_side == Some(DetectorSide::Source),
        out.reason,
    );
    let peer_extraction = extraction_json(
        &peer_bytes,
        out.peer_evidence.as_ref(),
        out.unavailable_side == Some(DetectorSide::Peer),
        out.reason,
    );
    let peer_extraction = if out.unavailable_side == Some(DetectorSide::Source) {
        json!({
            "state": "not_assessed",
            "reason": "source_extraction_failed_before_peer_assessment",
            "evidence": Value::Null
        })
    } else {
        peer_extraction
    };
    let mut comparison = comparison(
        decision_name(out.decision),
        out.reason,
        "detector_pairwise_comparison",
        detector_side(out.unavailable_side),
        vec![],
    );
    comparison["similarity"] = out.similarity.map_or(Value::Null, |score| json!(score));
    Ok(json!({
        "version": 1,
        "mode": "retained_html",
        "semantics": s06_detector::SEMANTICS,
        "provenance": {
            "input": "caller_supplied_files",
            "status_source": "caller_supplied",
            "content_type_source": "caller_supplied_or_html_default",
            "completeness_source": "fully_read_local_file",
            "source_limit_bytes": MAX_SOURCE_BYTES,
            "network_access": false,
            "raw_html_persistence": false,
            "report_or_delivery_write": false
        },
        "source": {
            "input": "retained_html",
            "file": source.display().to_string(),
            "file_sha256": seosmith::model::digest(&source_bytes),
            "bytes_read": source_bytes.len(),
            "source_limit_exceeded": source_bytes.len() > MAX_SOURCE_BYTES,
            "status": source_status,
            "content_type": source_content_type,
            "extraction": source_extraction
        },
        "peer": {
            "input": "retained_html",
            "file": peer.display().to_string(),
            "file_sha256": seosmith::model::digest(&peer_bytes),
            "bytes_read": peer_bytes.len(),
            "source_limit_exceeded": peer_bytes.len() > MAX_SOURCE_BYTES,
            "status": peer_status,
            "content_type": peer_content_type,
            "extraction": peer_extraction
        },
        "comparison": comparison
    }))
}

fn extraction_json(
    bytes: &[u8],
    evidence: Option<&Evidence>,
    unavailable: bool,
    reason: &str,
) -> Value {
    if let Some(evidence) = evidence {
        let summary = EvidenceSummary::from(evidence);
        json!({
            "state": if unavailable { "unavailable" } else { "extracted" },
            "reason": if unavailable { json!(reason) } else { Value::Null },
            "evidence": evidence_json(&summary)
        })
    } else {
        json!({
            "state": "unavailable",
            "reason": reason,
            "evidence": Value::Null,
            "input_sha256": seosmith::model::digest(bytes)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(source: &str, main: &str) -> EvidenceSummary {
        EvidenceSummary {
            source_sha256: seosmith::model::digest(source.as_bytes()),
            main_sha256: seosmith::model::digest(main.as_bytes()),
            title: "Title".into(),
            h1: "Heading".into(),
            excerpt: main.chars().take(240).collect(),
            template_fraction: 0.25,
        }
    }

    fn envelope(
        url: &str,
        decision: &str,
        reason: &str,
        evidence: Option<&EvidenceSummary>,
    ) -> String {
        envelope_with(url, s06_detector::SEMANTICS, decision, reason, evidence)
    }

    fn envelope_with(
        url: &str,
        semantics: &str,
        decision: &str,
        reason: &str,
        evidence: Option<&EvidenceSummary>,
    ) -> String {
        let capture = json!({
            "version": 1,
            "url": url,
            "collectors": [{
                "collector": "soft_404",
                "version": 1,
                "state": if decision == "unknown" { "partial" } else { "observed" },
                "reason": if decision == "unknown" { json!(reason) } else { Value::Null },
                "data": {
                    "semantics": semantics,
                    "decision": decision,
                    "reason": reason,
                    "evidence": evidence.map(evidence_json)
                }
            }],
            "omissions": []
        });
        let input = serde_json::to_string(&capture).unwrap();
        serde_json::to_string(&json!({
            "input_hash": seosmith::model::digest(input.as_bytes()),
            "capture": capture
        }))
        .unwrap()
    }

    #[test]
    fn identical_source_bytes_are_a_candidate_without_a_score() {
        let a = evidence("same", "alpha beta gamma");
        let b = evidence("same", "alpha beta gamma");
        let source = SavedSide::parse(
            "https://fixture.invalid/a",
            Some(200),
            &envelope(
                "https://fixture.invalid/a",
                "no_candidate",
                "no matching error pattern",
                Some(&a),
            ),
        );
        let peer = SavedSide::parse(
            "https://fixture.invalid/b",
            Some(200),
            &envelope(
                "https://fixture.invalid/b",
                "no_candidate",
                "no matching error pattern",
                Some(&b),
            ),
        );
        let comparison = compare_saved_sides(&source, &peer);
        assert_eq!(comparison["decision"], "candidate");
        assert_eq!(comparison["basis"], "identical_source_bytes");
        assert!(comparison["similarity"].is_null());
        assert_eq!(comparison["observations"]["main_text_hash_equal"], true);
    }

    #[test]
    fn identical_bytes_for_non_200_saved_sides_are_not_a_candidate() {
        let a = evidence("same", "alpha beta gamma");
        let b = evidence("same", "alpha beta gamma");
        let reason = "explicit missing status";
        let source = SavedSide::parse(
            "https://fixture.invalid/a",
            Some(404),
            &envelope(
                "https://fixture.invalid/a",
                "no_candidate",
                reason,
                Some(&a),
            ),
        );
        let peer = SavedSide::parse(
            "https://fixture.invalid/b",
            Some(404),
            &envelope(
                "https://fixture.invalid/b",
                "no_candidate",
                reason,
                Some(&b),
            ),
        );
        let comparison = compare_saved_sides(&source, &peer);
        assert_eq!(comparison["decision"], "unknown");
        assert_eq!(comparison["unavailable_side"], "source");
        assert_eq!(comparison["reason"], "saved_side_not_http_200");
        assert!(comparison.get("observations").is_none());
        assert_eq!(comparison["omissions"][0]["side"], "source");
        assert_eq!(comparison["omissions"][1]["side"], "peer");
        assert_eq!(source.to_json()["http_status"], 404);
        assert_ne!(comparison["decision"], "candidate");
    }

    #[test]
    fn a_200_side_is_not_byte_equal_to_a_non_200_saved_side() {
        let a = evidence("same", "alpha beta gamma");
        let b = evidence("same", "alpha beta gamma");
        let ok = SavedSide::parse(
            "https://fixture.invalid/ok",
            Some(200),
            &envelope(
                "https://fixture.invalid/ok",
                "no_candidate",
                "no matching error pattern",
                Some(&a),
            ),
        );
        let missing = SavedSide::parse(
            "https://fixture.invalid/missing",
            Some(404),
            &envelope(
                "https://fixture.invalid/missing",
                "no_candidate",
                "explicit missing status",
                Some(&b),
            ),
        );
        let comparison = compare_saved_sides(&ok, &missing);
        assert_eq!(comparison["decision"], "unknown");
        assert_eq!(comparison["unavailable_side"], "peer");
        assert_eq!(comparison["reason"], "saved_side_not_http_200");
    }

    #[test]
    fn a_missing_frontier_status_is_not_a_usable_side() {
        let a = evidence("same", "alpha beta gamma");
        let side = SavedSide::parse(
            "https://fixture.invalid/a",
            None,
            &envelope(
                "https://fixture.invalid/a",
                "no_candidate",
                "no matching error pattern",
                Some(&a),
            ),
        );
        assert_eq!(side.unusable.as_deref(), Some("saved_side_not_http_200"));
    }

    #[test]
    fn near_duplicate_saved_evidence_stays_unknown() {
        let a = evidence("left", "alpha beta gamma");
        let b = evidence("right", "alpha beta gamma delta");
        let source = SavedSide::parse(
            "https://fixture.invalid/a",
            Some(200),
            &envelope(
                "https://fixture.invalid/a",
                "no_candidate",
                "no matching error pattern",
                Some(&a),
            ),
        );
        let peer = SavedSide::parse(
            "https://fixture.invalid/b",
            Some(200),
            &envelope(
                "https://fixture.invalid/b",
                "no_candidate",
                "no matching error pattern",
                Some(&b),
            ),
        );
        let comparison = compare_saved_sides(&source, &peer);
        assert_eq!(comparison["decision"], "unknown");
        assert_eq!(comparison["basis"], "saved_evidence_insufficient");
        assert_eq!(comparison["observations"]["main_text_hash_equal"], false);
    }

    #[test]
    fn equal_main_hash_does_not_bypass_detector_support_or_sku_gates() {
        let a = evidence("left", "welcome");
        let b = evidence("right", "welcome");
        let source = SavedSide::parse(
            "https://fixture.invalid/a",
            Some(200),
            &envelope(
                "https://fixture.invalid/a",
                "no_candidate",
                "no matching error pattern",
                Some(&a),
            ),
        );
        let peer = SavedSide::parse(
            "https://fixture.invalid/b",
            Some(200),
            &envelope(
                "https://fixture.invalid/b",
                "no_candidate",
                "no matching error pattern",
                Some(&b),
            ),
        );
        let comparison = compare_saved_sides(&source, &peer);
        assert_eq!(comparison["decision"], "unknown");
        assert_eq!(comparison["observations"]["main_text_hash_equal"], true);
        assert_ne!(comparison["decision"], "candidate");
    }

    #[test]
    fn unknown_assessment_reports_its_side_and_reason() {
        let a = evidence("left", "");
        let shell = SavedSide::parse(
            "https://fixture.invalid/shell",
            Some(200),
            &envelope(
                "https://fixture.invalid/shell",
                "unknown",
                "main content or rendered outcome unavailable",
                Some(&a),
            ),
        );
        let page = SavedSide::parse(
            "https://fixture.invalid/page",
            Some(200),
            &envelope(
                "https://fixture.invalid/page",
                "no_candidate",
                "no matching error pattern",
                Some(&a),
            ),
        );
        let comparison = compare_saved_sides(&shell, &page);
        assert_eq!(comparison["decision"], "unknown");
        assert_eq!(comparison["unavailable_side"], "source");
        assert_eq!(
            comparison["reason"],
            "main content or rendered outcome unavailable"
        );
        let comparison = compare_saved_sides(&page, &shell);
        assert_eq!(comparison["unavailable_side"], "peer");
    }

    #[test]
    fn missing_incompatible_and_tampered_evidence_stays_unavailable() {
        let missing = SavedSide::parse("https://fixture.invalid/x", Some(200), "{}");
        assert_eq!(
            missing.unusable.as_deref(),
            Some("unsupported_evidence_envelope")
        );
        let a = evidence("left", "alpha beta gamma");
        let right = envelope(
            "https://fixture.invalid/a",
            "no_candidate",
            "no matching error pattern",
            Some(&a),
        );
        let wrong_url = SavedSide::parse("https://fixture.invalid/b", Some(200), &right);
        assert_eq!(
            wrong_url.unusable.as_deref(),
            Some("unsupported_or_unbound_capture")
        );
        let mut tampered: Value = serde_json::from_str(&right).unwrap();
        tampered["capture"]["url"] = json!("https://fixture.invalid/other");
        let tampered = SavedSide::parse(
            "https://fixture.invalid/other",
            Some(200),
            &serde_json::to_string(&tampered).unwrap(),
        );
        assert_eq!(
            tampered.unusable.as_deref(),
            Some("evidence_integrity_mismatch")
        );
        let old_semantics = envelope_with(
            "https://fixture.invalid/a",
            "s06-local-detector-1",
            "no_candidate",
            "no matching error pattern",
            Some(&a),
        );
        let incompatible = SavedSide::parse("https://fixture.invalid/a", Some(200), &old_semantics);
        assert_eq!(
            incompatible.unusable.as_deref(),
            Some("incompatible_saved_semantics")
        );
    }

    #[test]
    fn run_budget_marker_keeps_url_identity() {
        let url = "https://fixture.invalid/long";
        let marker = json!({
            "input_hash": "budget-marker-digest-is-not-the-replaced-capture",
            "capture": {
                "version": 1,
                "url": null,
                "url_omission": "run_byte_limit",
                "url_sha256": seosmith::model::digest(url.as_bytes()),
                "collectors": []
            }
        });
        let side = SavedSide::parse(url, Some(200), &serde_json::to_string(&marker).unwrap());
        assert_eq!(side.unusable.as_deref(), Some("run_byte_limit"));
        assert_eq!(
            side.to_json()["url_sha256"],
            seosmith::model::digest(url.as_bytes())
        );
        let wrong = SavedSide::parse(
            "https://fixture.invalid/other",
            Some(200),
            &serde_json::to_string(&marker).unwrap(),
        );
        assert_eq!(
            wrong.unusable.as_deref(),
            Some("unsupported_or_unbound_capture")
        );
    }
}
