//! Opt-in evidence metadata. Original observations/findings remain authoritative.
use crate::generation_error::{JobErrorCode, ensure_coded, ensure_invariant};
use crate::{
    audit,
    model::{Report, digest},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const MAX_OBSERVATIONS: usize = 128;
pub const MAX_EVALUATIONS: usize = 512;
pub const MAX_EXTENSION_BYTES: usize = 1024 * 1024;
pub const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;
pub const VALIDATOR_VERSION: &str = "s01-1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Policy {
    /// Exact UTF-8 bytes hashed below; not a cryptographic authenticity signature.
    pub canonical_json: String,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Interval {
    pub started_at: String,
    pub finished_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub entity_id: String,
    pub source_locator: String,
    pub state: Availability,
    pub unknown_reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    Eligible,
    Ineligible,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Attempt {
    Completed,
    NotAttempted,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    NotApplicable,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EvidenceValue {
    Source { source_locator: String },
    Known { value: serde_json::Value },
    Unavailable { reason: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evaluation {
    pub entity_id: String,
    pub rule: String,
    pub rule_version: String,
    pub validator_version: String,
    pub eligibility: Eligibility,
    pub attempt: Attempt,
    pub outcome: Outcome,
    pub unknown_reason: Option<String>,
    pub source_locator: String,
    pub actual: EvidenceValue,
    pub expected: EvidenceValue,
    pub confidence: Confidence,
    pub finding_key: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Coverage {
    pub rule: String,
    pub entities: usize,
    pub eligible: usize,
    pub ineligible: usize,
    pub eligibility_unknown: usize,
    pub attempted: usize,
    pub not_attempted: usize,
    pub attempt_unknown: usize,
    pub passed: usize,
    pub failed: usize,
    pub not_applicable: usize,
    pub unknown: usize,
    pub unknown_reasons: std::collections::BTreeMap<String, usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvidenceContract {
    pub version: u32,
    pub policy: Policy,
    pub comparison_key: String,
    /// Shared by every observation/evaluation; interval, never a fabricated fetch timestamp.
    pub observed_at: Interval,
    pub observations: Vec<Observation>,
    pub evaluations: Vec<Evaluation>,
    pub coverage: Vec<Coverage>,
    pub truncated: bool,
    pub omitted_observations: usize,
    pub evaluation_coverage_complete: bool,
}

/// Identity uses the exact stored URL (no further normalization) and resource kind.
pub fn entity_id(url: &str, resource: bool) -> String {
    let kind = if resource { "resource" } else { "page" };
    format!(
        "sha256:{}",
        digest(format!("seosmith-entity-v1\n{kind}\n{url}").as_bytes())
    )
}

/// Counting serialization prevents allocating another potentially large report string.
pub fn serialized_size(value: &impl Serialize) -> Result<usize> {
    #[derive(Default)]
    struct Counter {
        size: usize,
        depth: usize,
        string: bool,
        escaped: bool,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for &b in bytes {
                if self.string {
                    if self.escaped {
                        self.escaped = false;
                    } else if b == b'\\' {
                        self.escaped = true;
                    } else if b == b'"' {
                        self.string = false;
                    }
                } else {
                    match b {
                        b'"' => self.string = true,
                        b'{' | b'[' => {
                            self.depth += 1;
                            if self.depth > 64 {
                                return Err(std::io::Error::other(
                                    "report exceeds JSON nesting limit",
                                ));
                            }
                        }
                        b'}' | b']' => self.depth -= 1,
                        _ => {}
                    }
                }
            }
            self.size += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.size)
}

/// Called once, before persistence; never during delivery/retry.
pub fn enable_v2(report: &mut Report) -> Result<()> {
    ensure!(
        report.schema_version == 1 && report.evidence_contract.is_none(),
        "expected legacy report"
    );
    let contract = build_contract(report)?;
    report.schema_version = 2;
    report.evidence_contract = Some(contract);
    Ok(())
}

/// Attach S01 evidence v1 to a schema-4 candidate.
///
/// At the call site (after `policy::apply_with_evidence`, before the wire sets
/// `schema_version = 4`) the report is still labelled with the experimental
/// schema 3. Call it over the final (post-policy) findings array: every
/// `/findings/N` pointer refers to that exact array, so nothing may add,
/// remove or reorder findings afterwards. It never sets `schema_version`; the
/// caller does that when it builds the wire.
///
/// Every delivered finding of an `audit::rules()` rule that is not dropped by
/// `MAX_OBSERVATIONS` / `MAX_EVALUATIONS` gets a `fail` evaluation pointing at it.
pub fn attach_v1_schema4(report: &mut Report) -> Result<()> {
    ensure_invariant!(
        report.schema_version == 3
            && report.policy_guidance.is_some()
            && report.evidence_contract.is_none(),
        "expected policy report awaiting schema 4"
    );
    let contract = build_contract(report)?;
    report.evidence_contract = Some(contract);
    Ok(())
}

/// Shared evidence builder. Reads `report`, never mutates it.
fn build_contract(report: &Report) -> Result<EvidenceContract> {
    ensure_invariant!(
        report.pages.len() <= 40_000 && report.findings.len() <= 400_000,
        "report record limit"
    );
    ensure_invariant!(
        report.engine_version.len() <= 128
            && report.ruleset_version.len() <= 128
            && report.started_at.len() <= 64
            && report.finished_at.len() <= 64
            && report.request.url.as_str().len() <= 16_384
            && report
                .findings
                .iter()
                .all(|f| f.key.len() == 64 && f.key.bytes().all(|b| b.is_ascii_hexdigit())),
        "metadata scalar limit"
    );
    let base_size = serialized_size(report)?;
    ensure_coded!(
        JobErrorCode::ReportByteLimit,
        base_size < MAX_REPORT_BYTES,
        "report exceeds the 16 MiB report limit"
    );
    let r = &report.request;
    // BTreeMap-backed serde_json objects serialize keys in lexical order, no whitespace.
    let canonical_json = serde_json::to_string(&json!({
        "policy_version": "s01-1", "engine_version": report.engine_version,
        "ruleset_version": report.ruleset_version, "validator_version": VALIDATOR_VERSION,
        "seed_url": r.url, "max_pages": r.max_pages, "max_resources": r.max_resources,
        "max_duration_seconds": r.max_duration_seconds, "delay_ms": r.delay_ms,
        "check_external_links": r.check_external_links
    }))?;
    let signature = format!("sha256:{}", digest(canonical_json.as_bytes()));
    let mut contract = EvidenceContract {
        version: 1,
        comparison_key: signature.clone(),
        policy: Policy {
            canonical_json,
            signature,
        },
        observed_at: Interval {
            started_at: report.started_at.clone(),
            finished_at: report.finished_at.clone(),
        },
        observations: vec![],
        evaluations: vec![],
        coverage: vec![],
        truncated: false,
        omitted_observations: 0,
        evaluation_coverage_complete: false,
    };
    let mut identities = std::collections::HashSet::new();
    for page in &report.pages {
        ensure_invariant!(
            identities.insert(entity_id(&page.url, page.is_resource)),
            "duplicate entity identity"
        );
    }
    for (i, page) in report.pages.iter().take(MAX_OBSERVATIONS).enumerate() {
        let reason = if page.blocked_by_robots {
            Some("robots_blocked")
        } else if page.error.is_some() {
            Some("fetch_failed")
        } else if page.status.is_none() {
            Some("not_fetched")
        } else {
            None
        };
        contract.observations.push(Observation {
            entity_id: entity_id(&page.url, page.is_resource),
            source_locator: format!("/pages/{i}"),
            state: if reason.is_some() {
                Availability::Unavailable
            } else {
                Availability::Available
            },
            unknown_reason: reason.map(str::to_owned),
        });
    }
    for rule in audit::rules() {
        let mut c = Coverage {
            rule: rule.id.into(),
            entities: report.pages.len(),
            eligible: 0,
            ineligible: 0,
            eligibility_unknown: 0,
            attempted: 0,
            not_attempted: 0,
            attempt_unknown: 0,
            passed: 0,
            failed: 0,
            not_applicable: 0,
            unknown: 0,
            unknown_reasons: std::collections::BTreeMap::new(),
        };
        // At most one finding per rule/URL; a duplicate would make the pointer ambiguous.
        let mut findings = std::collections::HashMap::new();
        for (i, f) in report
            .findings
            .iter()
            .enumerate()
            .filter(|(_, f)| f.rule == rule.id)
        {
            ensure_invariant!(
                findings.insert(f.url.as_str(), (i, f)).is_none(),
                "duplicate finding for rule/URL"
            );
        }
        for (i, page) in report.pages.iter().enumerate() {
            let limited = i >= MAX_OBSERVATIONS || contract.evaluations.len() >= MAX_EVALUATIONS;
            let instrumented = rule.id == "missing_title";
            // A delivered finding always gets its pointer, whatever the page state.
            let finding = findings.get(page.url.as_str());
            if limited || (finding.is_none() && !instrumented) {
                c.eligibility_unknown += 1;
                c.attempt_unknown += 1;
                c.unknown += 1;
                *c.unknown_reasons
                    .entry(
                        if limited {
                            "contract_limit"
                        } else {
                            "legacy_not_instrumented"
                        }
                        .into(),
                    )
                    .or_default() += 1;
                contract.truncated |= limited;
                continue;
            }
            let mut e = Evaluation {
                entity_id: entity_id(&page.url, page.is_resource),
                rule: rule.id.into(),
                rule_version: report.ruleset_version.clone(),
                validator_version: VALIDATOR_VERSION.into(),
                eligibility: Eligibility::Eligible,
                attempt: Attempt::Completed,
                outcome: Outcome::Fail,
                unknown_reason: None,
                source_locator: format!("/pages/{i}"),
                actual: EvidenceValue::Source {
                    source_locator: format!("/pages/{i}/title"),
                },
                expected: EvidenceValue::Known {
                    value: "nonempty title".into(),
                },
                confidence: Confidence::High,
                finding_key: None,
            };
            if let Some((j, f)) = finding {
                e.source_locator = format!("/findings/{j}");
                e.actual = EvidenceValue::Source {
                    source_locator: format!("/findings/{j}/evidence_items"),
                };
                e.expected = EvidenceValue::Unavailable {
                    reason: "legacy_expectation_not_recorded".into(),
                };
                e.finding_key = Some(f.key.clone());
            } else if page.blocked_by_robots
                || page.error.is_some()
                || page.status.is_none()
                || page.extraction_truncated
            {
                let reason = if page.extraction_truncated {
                    "extraction_truncated"
                } else {
                    "source_unavailable"
                };
                e.eligibility = Eligibility::Unknown;
                e.attempt = Attempt::NotAttempted;
                e.outcome = Outcome::Unknown;
                e.unknown_reason = Some(reason.into());
                e.actual = EvidenceValue::Unavailable {
                    reason: reason.into(),
                };
                e.confidence = Confidence::Unknown;
            } else if !page.html() || page.is_resource {
                e.eligibility = Eligibility::Ineligible;
                e.attempt = Attempt::NotAttempted;
                e.outcome = Outcome::NotApplicable;
                e.actual = EvidenceValue::Unavailable {
                    reason: "not_html_page".into(),
                };
            } else {
                e.outcome = if page.title.is_empty() {
                    Outcome::Fail
                } else {
                    Outcome::Pass
                };
            }
            match e.eligibility {
                Eligibility::Eligible => c.eligible += 1,
                Eligibility::Ineligible => c.ineligible += 1,
                Eligibility::Unknown => c.eligibility_unknown += 1,
            }
            match e.attempt {
                Attempt::Completed => c.attempted += 1,
                Attempt::NotAttempted => c.not_attempted += 1,
                Attempt::Unknown => c.attempt_unknown += 1,
            }
            match e.outcome {
                Outcome::Pass => c.passed += 1,
                Outcome::Fail => c.failed += 1,
                Outcome::NotApplicable => c.not_applicable += 1,
                Outcome::Unknown => {
                    c.unknown += 1;
                    *c.unknown_reasons
                        .entry(
                            e.unknown_reason
                                .clone()
                                .context("unknown evaluation requires reason")?,
                        )
                        .or_default() += 1;
                }
            }
            contract.evaluations.push(e);
        }
        contract.coverage.push(c);
    }
    contract.omitted_observations = report.pages.len() - contract.observations.len();
    contract.evaluation_coverage_complete = report.coverage_complete
        && !contract.truncated
        && contract.coverage.iter().all(|c| c.unknown == 0);
    ensure_coded!(
        JobErrorCode::EvidenceExtensionLimit,
        serialized_size(&contract)? <= MAX_EXTENSION_BYTES,
        "evidence extension exceeds limit"
    );
    // Reject oversized final reports before changing the original or allocating serialized bytes.
    ensure_coded!(
        JobErrorCode::ReportByteLimit,
        base_size + serialized_size(&contract)? + 32 <= MAX_REPORT_BYTES,
        "versioned report exceeds the 16 MiB report limit"
    );
    Ok(contract)
}
