//! Local candidate validation only. This is not a receiver or a frozen wire contract.
use crate::{
    contract,
    model::{Report, digest},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub record_id: String,
    pub category: String,
    pub rule_id: String,
    pub rule_version: String,
    pub entity_id: String,
    pub discriminator: String,
    pub eligibility: String,
    pub attempt: String,
    pub outcome: String,
    pub actual: String,
    pub source_locator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_locator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_locator: Option<String>,
    pub expected: Option<String>,
    pub confidence: String,
    pub interval_ref: String,
    pub policy_ref: String,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub category: String,
    pub rule_id: String,
    pub candidates: usize,
    pub emitted: usize,
    pub omitted: usize,
    pub complete: bool,
    pub eligibility_counts: BTreeMap<String, usize>,
    pub attempt_counts: BTreeMap<String, usize>,
    pub outcomes: BTreeMap<String, usize>,
    pub unknown_reasons: BTreeMap<String, usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRow {
    pub category: String,
    pub rule_id: String,
    pub rule_version: String,
    pub expected_pointer: Option<String>,
    pub candidates: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub policy_canonical_json: String,
    pub policy_digest: String,
    pub records: Vec<Record>,
    pub coverage: Vec<Coverage>,
}
fn component(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && s.bytes().all(|c| (0x20..=0x7e).contains(&c))
}
fn valid_pointer(pointer: &str) -> bool {
    if !pointer.starts_with('/') || pointer.len() > 1024 {
        return false;
    }
    let bytes = pointer.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'~' {
            if i + 1 == bytes.len() || !matches!(bytes[i + 1], b'0' | b'1') {
                return false;
            }
            i += 1;
        }
        i += 1;
    }
    true
}
pub fn record_id(r: &Record) -> String {
    format!(
        "sha256:{}",
        digest(
            format!(
                "seosmith-record-v1\n{}\n{}\n{}\n{}\n{}",
                r.category, r.rule_id, r.rule_version, r.entity_id, r.discriminator
            )
            .as_bytes()
        )
    )
}
/// Validates bounded, local candidate vectors against their original report. Not suitable for accepting external bytes.
#[allow(clippy::items_after_statements)]
pub fn validate(candidate: &Candidate, report: &Report, manifest: &[ManifestRow]) -> Result<()> {
    ensure!(
        candidate.policy_canonical_json.len() <= 65536,
        "policy limit"
    );
    let value: serde_json::Value = serde_json::from_str(&candidate.policy_canonical_json)?;
    fn integers(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Number(n) => n
                .as_i64()
                .is_some_and(|x| (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&x)),
            serde_json::Value::Array(a) => a.iter().all(integers),
            serde_json::Value::Object(o) => o.values().all(integers),
            _ => true,
        }
    }
    ensure!(
        value.is_object()
            && integers(&value)
            && serde_json::to_string(&value)? == candidate.policy_canonical_json,
        "noncanonical policy"
    );
    ensure!(
        candidate.policy_digest
            == format!(
                "sha256:{}",
                digest(candidate.policy_canonical_json.as_bytes())
            ),
        "policy digest"
    );
    ensure!(
        manifest.len() <= 256
            && candidate.coverage.len() == manifest.len()
            && candidate.records.len() <= 512,
        "count limit"
    );
    ensure!(
        serde_json::to_vec(manifest)?.len() <= 262_144,
        "manifest byte limit"
    );
    let mut manifest_keys = HashSet::new();
    for row in manifest {
        ensure!(
            component(&row.category)
                && component(&row.rule_id)
                && component(&row.rule_version)
                && manifest_keys.insert((&row.category, &row.rule_id)),
            "invalid manifest"
        );
        let mut unique = HashSet::new();
        ensure!(
            row.candidates.iter().all(|id| unique.insert(id)
                && report.pages.iter().any(|p| {
                    let entity = contract::entity_id(&p.url, p.is_resource);
                    if *id == entity {
                        return true;
                    }
                    id.strip_prefix(&(entity + "|edge:"))
                        .and_then(|n| n.parse::<usize>().ok().filter(|i| i.to_string() == n))
                        .is_some_and(|i| i < p.links.len())
                })),
            "invalid candidate universe"
        );
    }
    let matrix: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/policies/state-matrix.json"))?;
    let tuples = matrix["tuples"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid matrix"))?;
    let mut ids = HashSet::new();
    let original = serde_json::to_value(report)?;
    for r in &candidate.records {
        ensure!(
            [
                &r.category,
                &r.rule_id,
                &r.rule_version,
                &r.entity_id,
                &r.discriminator
            ]
            .iter()
            .all(|s| component(s)),
            "invalid identity component"
        );
        ensure!(
            r.record_id == record_id(r) && ids.insert(&r.record_id),
            "record identity"
        );
        let rule = manifest
            .iter()
            .find(|m| m.category == r.category && m.rule_id == r.rule_id)
            .ok_or_else(|| anyhow::anyhow!("unknown rule"))?;
        ensure!(
            rule.rule_version == r.rule_version
                && rule.candidates.contains(&if r.discriminator == "self" {
                    r.entity_id.clone()
                } else {
                    format!("{}|{}", r.entity_id, r.discriminator)
                })
                && (r.attempt != "completed" || r.expected == rule.expected_pointer),
            "rule or scope mismatch"
        );
        ensure!(
            r.discriminator == "self"
                || (r.category == "observation"
                    && r.rule_id == "external_link"
                    && r.discriminator.starts_with("edge:")),
            "invalid edge scope"
        );
        if r.discriminator != "self" {
            ensure!(r.context_locator.is_some(), "edge context required");
            let edge = r
                .context_locator
                .as_ref()
                .and_then(|p| original.pointer(p))
                .and_then(|v| v.get("url"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("edge target absent"))?;
            if r.outcome == "observed" {
                let target = r
                    .target_locator
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("target provenance required"))?;
                let parts: Vec<_> = target.split('/').collect();
                ensure!(
                    parts.len() == 4
                        && parts[0].is_empty()
                        && parts[1] == "pages"
                        && parts[3] == "status",
                    "target pointer"
                );
                let n: usize = parts[2].parse()?;
                ensure!(
                    parts[2] == n.to_string()
                        && report.pages.get(n).is_some_and(|p| p.url == edge
                            && p.status.is_some()
                            && p.error.is_none()
                            && !p.blocked_by_robots),
                    "target mismatch"
                );
            } else {
                ensure!(r.target_locator.is_none(), "unknown target provenance");
            }
        } else {
            ensure!(r.target_locator.is_none(), "unexpected target provenance");
        }
        let row = tuples
            .iter()
            .find(|t| {
                t["category"] == r.category
                    && t["eligibility"] == r.eligibility
                    && t["attempt"] == r.attempt
                    && t["outcome"] == r.outcome
                    && t["actual"] == r.actual
            })
            .ok_or_else(|| anyhow::anyhow!("invalid tuple"))?;
        ensure!(
            row["confidence"] == r.confidence
                && r.interval_ref == "report.observed_at"
                && r.policy_ref == candidate.policy_digest,
            "confidence or inheritance"
        );
        ensure!(
            if row["expected"] == "policy_source" {
                r.expected
                    .as_ref()
                    .is_some_and(|p| valid_pointer(p) && value.pointer(p).is_some())
            } else {
                r.expected.is_none()
            },
            "expected policy source"
        );
        let needs_reason = row["reason"] == "required";
        ensure!(
            if needs_reason {
                r.reason.as_ref().is_some_and(|s| component(s)) && r.source_locator.is_none()
            } else {
                r.reason.is_none() && r.source_locator.is_some()
            },
            "invalid provenance"
        );
        for pointer in r.source_locator.iter().chain(r.context_locator.iter()) {
            ensure!(valid_pointer(pointer), "invalid source pointer");
            let parts: Vec<_> = pointer.split('/').collect();
            ensure!(
                parts.len() >= 3 && parts[0].is_empty() && parts[1] == "pages",
                "source pointer"
            );
            let i: usize = parts[2].parse()?;
            ensure!(parts[2] == i.to_string(), "noncanonical index");
            let page = report
                .pages
                .get(i)
                .ok_or_else(|| anyhow::anyhow!("page absent"))?;
            ensure!(
                r.entity_id == contract::entity_id(&page.url, page.is_resource),
                "source entity mismatch"
            );
            ensure!(original.pointer(pointer).is_some(), "source missing");
            ensure!(
                r.discriminator == "self"
                    || (parts.len() >= 5
                        && parts[3] == "links"
                        && r.discriminator == format!("edge:{}", parts[4])),
                "relationship discriminator"
            );
        }
    }
    ensure!(
        serde_json::to_vec(&candidate.coverage)?.len() <= 262_144,
        "coverage byte limit"
    );
    let mut covered = HashSet::new();
    for c in &candidate.coverage {
        ensure!(
            manifest.iter().any(|m| m.category == c.category
                && m.rule_id == c.rule_id
                && m.candidates.len() == c.candidates)
                && covered.insert((&c.category, &c.rule_id)),
            "coverage manifest"
        );
        let selected: Vec<_> = candidate
            .records
            .iter()
            .filter(|r| r.category == c.category && r.rule_id == c.rule_id)
            .collect();
        ensure!(
            !c.complete
                || (report.coverage_complete
                    && c.omitted == 0
                    && *c.outcomes.get("unknown").unwrap_or(&0) == 0),
            "invalid completeness"
        );
        ensure!(
            selected.len() == c.emitted && c.emitted.checked_add(c.omitted) == Some(c.candidates),
            "coverage total"
        );
        for (counts, allowed) in [
            (
                &c.eligibility_counts,
                &["eligible", "ineligible", "unknown"][..],
            ),
            (
                &c.attempt_counts,
                &["completed", "not_attempted", "unknown"][..],
            ),
        ] {
            ensure!(
                counts.keys().all(|k| allowed.contains(&k.as_str()))
                    && counts.values().try_fold(0usize, |a, b| a.checked_add(*b))
                        == Some(c.candidates),
                "coverage states"
            );
        }
        for (counts, get) in [(&c.eligibility_counts, 0), (&c.attempt_counts, 1)] {
            let mut seen = BTreeMap::<&str, usize>::new();
            for r in &selected {
                *seen
                    .entry(if get == 0 { &r.eligibility } else { &r.attempt })
                    .or_default() += 1;
            }
            ensure!(
                seen.iter()
                    .all(|(k, v)| counts.get(*k).is_some_and(|n| n >= v)),
                "coverage state reconciliation"
            );
        }
        ensure!(
            c.outcomes
                .values()
                .try_fold(0usize, |a, b| a.checked_add(*b))
                == Some(c.candidates),
            "outcome total"
        );
        let mut counts = BTreeMap::<&str, usize>::new();
        for r in &selected {
            *counts.entry(&r.outcome).or_default() += 1;
        }
        ensure!(
            counts
                .iter()
                .all(|(k, v)| c.outcomes.get(*k).is_some_and(|n| n >= v)),
            "outcome reconciliation"
        );
        ensure!(
            c.outcomes.keys().all(|outcome| tuples
                .iter()
                .any(|t| t["category"] == c.category && t["outcome"] == *outcome)),
            "unknown outcome"
        );
        ensure!(
            c.unknown_reasons.len() <= 64
                && c.unknown_reasons
                    .keys()
                    .all(|reason| !reason.is_empty() && reason.len() <= 256),
            "reason bounds"
        );
        let mut emitted_reasons = BTreeMap::<&str, usize>::new();
        for record in &selected {
            if record.outcome == "unknown"
                && let Some(reason) = &record.reason
            {
                *emitted_reasons.entry(reason).or_default() += 1;
            }
        }
        ensure!(
            emitted_reasons.iter().all(|(key, count)| c
                .unknown_reasons
                .get(*key)
                .is_some_and(|total| total >= count)),
            "unknown reason reconciliation"
        );
        ensure!(
            c.unknown_reasons
                .values()
                .try_fold(0usize, |a, b| a.checked_add(*b))
                == Some(*c.outcomes.get("unknown").unwrap_or(&0)),
            "unknown reasons"
        );
    }
    Ok(())
}

/// Conservative lifecycle rule: an edge or incomplete evaluation never resolves a violation.
#[allow(clippy::fn_params_excessive_bools)]
pub fn resolves(
    previous: &Record,
    current: &Record,
    policy_equal: bool,
    manifest_equal: bool,
    crawl_complete: bool,
    relevant_coverage_complete: bool,
) -> bool {
    policy_equal
        && manifest_equal
        && crawl_complete
        && relevant_coverage_complete
        && previous.category == "policy_violation"
        && previous.outcome == "violation"
        && previous.discriminator == "self"
        && current.discriminator == "self"
        && previous.record_id == current.record_id
        && previous.rule_version == current.rule_version
        && current.category == "policy_violation"
        && current.eligibility == "eligible"
        && current.attempt == "completed"
        && current.outcome == "no_violation"
}

/// Produce a bounded experimental contract from the fixed S14 scalar rule set.
pub fn generate(
    report: &Report,
    policy: &crate::policy::AdvisoryPolicy,
) -> Result<(Candidate, Vec<ManifestRow>)> {
    policy.validate()?;
    let policy_json = serde_json::to_string(
        &serde_json::json!({"catalog_version":"s14-1","policy":policy,"request":report.request,"schema_version":3,"validator_version":"s14-1"}),
    )?;
    let signature = format!("sha256:{}", digest(policy_json.as_bytes()));
    let defs = [
        ("advisory", "short_title", "title_min", "title"),
        ("advisory", "long_title", "title_max", "title"),
        (
            "advisory",
            "short_description",
            "description_min",
            "description",
        ),
        (
            "advisory",
            "long_description",
            "description_max",
            "description",
        ),
        ("advisory", "little_content", "words_min", "words"),
        ("advisory", "too_many_links", "links_max", "links"),
        ("observation", "noindex", "", "robots"),
        ("observation", "http_redirect", "", "status"),
        ("observation", "missing_hreflang_xdefault", "", "alternates"),
        ("observation", "external_target", "", "status"),
        ("policy_violation", "noindex", "require_indexable", "robots"),
        (
            "policy_violation",
            "http_redirect",
            "forbid_redirects",
            "status",
        ),
    ];
    let mut contract = Candidate {
        policy_canonical_json: policy_json,
        policy_digest: signature.clone(),
        records: vec![],
        coverage: vec![],
    };
    let mut manifest = Vec::new();
    for (category, rule, key, field) in defs {
        let mut coverage = Coverage {
            category: category.into(),
            rule_id: rule.into(),
            candidates: 0,
            emitted: 0,
            omitted: 0,
            complete: false,
            eligibility_counts: BTreeMap::new(),
            attempt_counts: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            unknown_reasons: BTreeMap::new(),
        };
        let expected = if category == "observation" {
            None
        } else {
            Some(format!("/policy/{key}"))
        };
        let mut ids = Vec::new();
        for (i, page) in report.pages.iter().enumerate() {
            let in_scope = url::Url::parse(&page.url).is_ok_and(|u| report.request.in_scope(&u));
            if category == "observation"
                && ((rule == "external_target" && in_scope)
                    || (rule != "external_target" && (!in_scope || page.is_resource)))
            {
                continue;
            }
            coverage.candidates += 1;
            let entity = contract::entity_id(&page.url, page.is_resource);
            ids.push(entity.clone());
            let unavailable = page.blocked_by_robots
                || page.error.is_some()
                || page.status.is_none()
                || page.extraction_truncated
                || (category == "observation"
                    && matches!(rule, "noindex" | "missing_hreflang_xdefault")
                    && !page.html());
            let applicable = in_scope
                && !page.is_resource
                && (rule == "http_redirect" || page.html())
                && (rule != "noindex" || policy.require_indexable)
                && (rule != "http_redirect" || policy.forbid_redirects)
                && !(matches!(rule, "short_title" | "long_title") && page.title.is_empty())
                && !(matches!(rule, "short_description" | "long_description")
                    && page.description.is_empty());
            let (eligibility, attempt, outcome, reason) = if unavailable {
                (
                    "unknown",
                    "not_attempted",
                    "unknown",
                    Some("source_unavailable"),
                )
            } else if category == "observation" {
                ("eligible", "completed", "observed", None)
            } else if !applicable {
                (
                    "ineligible",
                    "not_attempted",
                    "not_applicable",
                    Some("outside_applicability"),
                )
            } else {
                let hit = match rule {
                    "short_title" => page.title.chars().count() < policy.title_min,
                    "long_title" => page.title.chars().count() > policy.title_max,
                    "short_description" => {
                        page.description.chars().count() < policy.description_min
                    }
                    "long_description" => page.description.chars().count() > policy.description_max,
                    "little_content" => page.words < policy.words_min,
                    "too_many_links" => page.links.len() > policy.links_max,
                    "noindex" => page.noindex(),
                    "http_redirect" => page.status.is_some_and(|s| (300..400).contains(&s)),
                    _ => false,
                };
                (
                    "eligible",
                    "completed",
                    if category == "advisory" {
                        if hit {
                            "advisory_triggered"
                        } else {
                            "advisory_not_triggered"
                        }
                    } else if hit {
                        "violation"
                    } else {
                        "no_violation"
                    },
                    None,
                )
            };
            *coverage
                .eligibility_counts
                .entry(eligibility.into())
                .or_default() += 1;
            *coverage.attempt_counts.entry(attempt.into()).or_default() += 1;
            *coverage.outcomes.entry(outcome.into()).or_default() += 1;
            if let Some(reason) = reason.filter(|_| outcome == "unknown") {
                *coverage.unknown_reasons.entry(reason.into()).or_default() += 1;
            }
            if contract.records.len() >= 512 {
                coverage.omitted += 1;
                continue;
            }
            let mut record = Record {
                record_id: String::new(),
                category: category.into(),
                rule_id: rule.into(),
                rule_version: "s14-1".into(),
                entity_id: entity,
                discriminator: "self".into(),
                eligibility: eligibility.into(),
                attempt: attempt.into(),
                outcome: outcome.into(),
                actual: if attempt == "completed" {
                    "source"
                } else {
                    "unavailable"
                }
                .into(),
                source_locator: if attempt == "completed" {
                    Some(format!("/pages/{i}/{field}"))
                } else {
                    None
                },
                context_locator: None,
                target_locator: None,
                expected: if attempt == "completed" {
                    expected.clone()
                } else {
                    None
                },
                confidence: if attempt == "completed" {
                    "high"
                } else {
                    "unknown"
                }
                .into(),
                interval_ref: "report.observed_at".into(),
                policy_ref: signature.clone(),
                reason: reason.map(str::to_owned),
            };
            record.record_id = record_id(&record);
            contract.records.push(record);
            coverage.emitted += 1;
        }
        manifest.push(ManifestRow {
            category: category.into(),
            rule_id: rule.into(),
            rule_version: "s14-1".into(),
            expected_pointer: expected,
            candidates: ids,
        });
        coverage.complete = report.coverage_complete
            && coverage.omitted == 0
            && *coverage.outcomes.get("unknown").unwrap_or(&0) == 0;
        contract.coverage.push(coverage);
    }
    // Each outgoing external link is a separate report-local edge, including unfetched targets.
    let mut edge_manifest = ManifestRow {
        category: "observation".into(),
        rule_id: "external_link".into(),
        rule_version: "s14-1".into(),
        expected_pointer: None,
        candidates: vec![],
    };
    let mut edge_coverage = Coverage {
        category: "observation".into(),
        rule_id: "external_link".into(),
        candidates: 0,
        emitted: 0,
        omitted: 0,
        complete: false,
        eligibility_counts: BTreeMap::new(),
        attempt_counts: BTreeMap::new(),
        outcomes: BTreeMap::new(),
        unknown_reasons: BTreeMap::new(),
    };
    for (page_index, page) in report.pages.iter().enumerate() {
        if page.is_resource
            || !url::Url::parse(&page.url).is_ok_and(|u| report.request.in_scope(&u))
        {
            continue;
        }
        for (link_index, link) in page.links.iter().enumerate() {
            if !url::Url::parse(&link.url).is_ok_and(|u| !report.request.in_scope(&u)) {
                continue;
            }
            let entity = contract::entity_id(&page.url, page.is_resource);
            let discriminator = format!("edge:{link_index}");
            edge_manifest
                .candidates
                .push(format!("{entity}|{discriminator}"));
            edge_coverage.candidates += 1;
            let target_index = report.pages.iter().position(|p| p.url == link.url);
            let target = target_index.and_then(|i| report.pages.get(i));
            let available = !page.extraction_truncated
                && !page.blocked_by_robots
                && page.error.is_none()
                && target.is_some_and(|p| {
                    p.status.is_some() && p.error.is_none() && !p.blocked_by_robots
                });
            let (eligibility, attempt, outcome, reason) = if available {
                ("eligible", "completed", "observed", None)
            } else {
                (
                    "unknown",
                    "not_attempted",
                    "unknown",
                    Some("external_target_unavailable"),
                )
            };
            *edge_coverage
                .eligibility_counts
                .entry(eligibility.into())
                .or_default() += 1;
            *edge_coverage
                .attempt_counts
                .entry(attempt.into())
                .or_default() += 1;
            *edge_coverage.outcomes.entry(outcome.into()).or_default() += 1;
            if let Some(reason) = reason {
                *edge_coverage
                    .unknown_reasons
                    .entry(reason.into())
                    .or_default() += 1;
            }
            if contract.records.len() >= 512 {
                edge_coverage.omitted += 1;
                continue;
            }
            let pointer = format!("/pages/{page_index}/links/{link_index}");
            let mut record = Record {
                record_id: String::new(),
                category: "observation".into(),
                rule_id: "external_link".into(),
                rule_version: "s14-1".into(),
                entity_id: entity,
                discriminator,
                eligibility: eligibility.into(),
                attempt: attempt.into(),
                outcome: outcome.into(),
                actual: if available { "source" } else { "unavailable" }.into(),
                source_locator: if available {
                    Some(pointer.clone())
                } else {
                    None
                },
                context_locator: Some(pointer),
                target_locator: if available {
                    target_index.map(|i| format!("/pages/{i}/status"))
                } else {
                    None
                },
                expected: None,
                confidence: if available { "high" } else { "unknown" }.into(),
                interval_ref: "report.observed_at".into(),
                policy_ref: signature.clone(),
                reason: reason.map(str::to_owned),
            };
            record.record_id = record_id(&record);
            contract.records.push(record);
            edge_coverage.emitted += 1;
        }
    }
    edge_coverage.complete = report.coverage_complete
        && edge_coverage.omitted == 0
        && *edge_coverage.outcomes.get("unknown").unwrap_or(&0) == 0;
    contract.coverage.push(edge_coverage);
    manifest.push(edge_manifest);
    validate(&contract, report, &manifest)?;
    Ok((contract, manifest))
}

/// Independently recompute the fixed manifest, scope and records from the report and effective policy.
pub fn validate_generated(
    candidate: &Candidate,
    manifest: &[ManifestRow],
    report: &Report,
    policy: &crate::policy::AdvisoryPolicy,
) -> Result<()> {
    let (expected, expected_manifest) = generate(report, policy)?;
    ensure!(
        serde_json::to_value(candidate)? == serde_json::to_value(expected)?
            && serde_json::to_value(manifest)? == serde_json::to_value(expected_manifest)?,
        "generated contract differs from authoritative rules or request scope"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> (Candidate, Report) {
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/contracts/legacy.json")).unwrap();
        let mut r = Record {
            record_id: String::new(),
            category: "advisory".into(),
            rule_id: "short_title".into(),
            rule_version: "s14-1".into(),
            entity_id: contract::entity_id(&report.pages[0].url, report.pages[0].is_resource),
            discriminator: "self".into(),
            eligibility: "eligible".into(),
            attempt: "completed".into(),
            outcome: "advisory_triggered".into(),
            actual: "source".into(),
            source_locator: Some("/pages/0/title".into()),
            context_locator: None,
            target_locator: None,
            expected: Some("/threshold".into()),
            confidence: "high".into(),
            interval_ref: "report.observed_at".into(),
            policy_ref: format!("sha256:{}", digest(br#"{"threshold":15}"#)),
            reason: None,
        };
        r.record_id = record_id(&r);
        let c = Candidate {
            policy_canonical_json: r#"{"threshold":15}"#.into(),
            policy_digest: format!("sha256:{}", digest(br#"{"threshold":15}"#)),
            records: vec![r],
            coverage: vec![Coverage {
                category: "advisory".into(),
                rule_id: "short_title".into(),
                candidates: 1,
                emitted: 1,
                omitted: 0,
                complete: false,
                eligibility_counts: BTreeMap::from([("eligible".into(), 1)]),
                attempt_counts: BTreeMap::from([("completed".into(), 1)]),
                outcomes: BTreeMap::from([("advisory_triggered".into(), 1)]),
                unknown_reasons: BTreeMap::new(),
            }],
        };
        (c, report)
    }
    #[test]
    fn producer_generates_scoped_advisories_and_unknowns() {
        let (_, mut report) = sample();
        report.pages[0].status = Some(200);
        report.pages[0].content_type = "text/html".into();
        report.pages[0].title = "short".into();
        report.pages[0].error = None;
        report.pages[0].blocked_by_robots = false;
        report.pages[0].extraction_truncated = false;
        let (candidate, manifest) =
            generate(&report, &crate::policy::AdvisoryPolicy::default()).unwrap();
        assert_eq!(manifest.len(), 13);
        assert!(
            validate_generated(
                &candidate,
                &manifest,
                &report,
                &crate::policy::AdvisoryPolicy::default()
            )
            .is_ok()
        );
        let mut wrong = manifest.clone();
        wrong.pop();
        assert!(
            validate_generated(
                &candidate,
                &wrong,
                &report,
                &crate::policy::AdvisoryPolicy::default()
            )
            .is_err()
        );
        assert!(
            candidate
                .records
                .iter()
                .any(|r| r.rule_id == "short_title" && r.outcome == "advisory_triggered")
        );
        assert!(
            candidate
                .records
                .iter()
                .any(|r| r.rule_id == "noindex" && r.outcome == "not_applicable")
        );
        report.pages[0].extraction_truncated = true;
        let (candidate, _) = generate(&report, &crate::policy::AdvisoryPolicy::default()).unwrap();
        assert!(
            candidate
                .records
                .iter()
                .any(|r| r.rule_id == "short_title" && r.outcome == "unknown")
        );
    }
    #[test]
    fn external_edges_preserve_missing_and_available_targets_without_crossrun_resolution() {
        let (_, mut report) = sample();
        let page = &mut report.pages[0];
        page.status = Some(200);
        page.content_type = "text/html".into();
        page.error = None;
        page.blocked_by_robots = false;
        page.extraction_truncated = false;
        page.links = vec![
            crate::model::Link {
                url: "https://outside.example/a".into(),
                ..Default::default()
            },
            crate::model::Link {
                url: "https://outside.example/b".into(),
                ..Default::default()
            },
        ];
        report.pages.push(crate::model::Page {
            url: "https://outside.example/b".into(),
            status: Some(200),
            ..Default::default()
        });
        let policy = crate::policy::AdvisoryPolicy::default();
        let (candidate, manifest) = generate(&report, &policy).unwrap();
        let edges: Vec<_> = candidate
            .records
            .iter()
            .filter(|r| r.rule_id == "external_link")
            .collect();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].outcome, "unknown");
        assert_eq!(
            edges[0].context_locator.as_deref(),
            Some("/pages/0/links/0")
        );
        assert_eq!(edges[1].outcome, "observed");
        assert_ne!(edges[0].record_id, edges[1].record_id);
        let mut bad = candidate.clone();
        let edge = bad
            .records
            .iter_mut()
            .find(|r| r.rule_id == "external_link")
            .unwrap();
        edge.context_locator = Some("/pages/0/links/1".into());
        assert!(validate(&bad, &report, &manifest).is_err());
        report.pages[0].extraction_truncated = true;
        let (partial, _) = generate(&report, &policy).unwrap();
        assert!(
            partial
                .records
                .iter()
                .filter(|r| r.rule_id == "external_link")
                .all(|r| r.outcome == "unknown")
        );
    }
    #[test]
    fn omitted_records_are_counted_and_never_complete() {
        let (_, mut report) = sample();
        let original = report.pages[0].clone();
        for i in 0..55 {
            let mut page = original.clone();
            page.url = format!("https://example.com/item-{i}");
            report.pages.push(page);
        }
        let (candidate, manifest) =
            generate(&report, &crate::policy::AdvisoryPolicy::default()).unwrap();
        assert_eq!(candidate.records.len(), 512);
        assert!(candidate.coverage.iter().any(|c| c.omitted > 0));
        assert!(
            candidate
                .coverage
                .iter()
                .filter(|c| c.omitted > 0)
                .all(|c| !c.complete)
        );
        assert!(
            validate_generated(
                &candidate,
                &manifest,
                &report,
                &crate::policy::AdvisoryPolicy::default()
            )
            .is_ok()
        );
    }
    #[test]
    fn observations_keep_intent_and_unavailable_targets_separate() {
        let (_, mut report) = sample();
        let page = &mut report.pages[0];
        page.status = Some(200);
        page.content_type = "text/html".into();
        page.robots = "noindex".into();
        page.error = None;
        page.blocked_by_robots = false;
        page.extraction_truncated = false;
        let mut external = crate::model::Page {
            url: "https://outside.example/".into(),
            ..Default::default()
        };
        external.error = Some("fetch_failed".into());
        report.pages.push(external);
        let (c, _) = generate(&report, &crate::policy::AdvisoryPolicy::default()).unwrap();
        assert!(c.records.iter().any(|r| r.category == "observation"
            && r.rule_id == "noindex"
            && r.outcome == "observed"));
        assert!(
            c.records
                .iter()
                .any(|r| r.rule_id == "missing_hreflang_xdefault"
                    && r.source_locator.as_deref() == Some("/pages/0/alternates"))
        );
        assert!(
            c.records
                .iter()
                .any(|r| r.rule_id == "external_target" && r.outcome == "unknown")
        );
        report.pages[0].extraction_truncated = true;
        let (c, _) = generate(&report, &crate::policy::AdvisoryPolicy::default()).unwrap();
        assert!(
            c.records
                .iter()
                .any(|r| r.rule_id == "missing_hreflang_xdefault" && r.outcome == "unknown")
        );
    }
    #[test]
    fn generated_interchange_fixture_and_negative_mutations() {
        let report: Report =
            serde_json::from_str(include_str!("../fixtures/policies/v3-local-complete.json"))
                .unwrap();
        let guidance = report.policy_guidance.as_ref().unwrap();
        let candidate = guidance.candidate.as_ref().unwrap();
        let manifest = guidance.manifest.as_ref().unwrap();
        assert!(validate_generated(candidate, manifest, &report, &guidance.config).is_ok());
        let mut changed = candidate.clone();
        changed.records[0].outcome = "violation".into();
        assert!(validate_generated(&changed, manifest, &report, &guidance.config).is_err());
        let mut changed_manifest = manifest.clone();
        changed_manifest.pop();
        assert!(
            validate_generated(candidate, &changed_manifest, &report, &guidance.config).is_err()
        );
        let partial: Report =
            serde_json::from_str(include_str!("../fixtures/policies/v3-local-partial.json"))
                .unwrap();
        let partial_guidance = partial.policy_guidance.as_ref().unwrap();
        let partial_candidate = partial_guidance.candidate.as_ref().unwrap();
        assert!(
            validate_generated(
                partial_candidate,
                partial_guidance.manifest.as_ref().unwrap(),
                &partial,
                &partial_guidance.config
            )
            .is_ok()
        );
        assert!(partial_candidate.coverage.iter().all(|row| !row.complete));
        let mut altered = guidance.config.clone();
        altered.title_min += 1;
        assert!(validate_generated(candidate, manifest, &report, &altered).is_err());
    }
    #[test]
    fn generated_policy_identity_and_explicit_intent() {
        let (_, mut report) = sample();
        let page = &mut report.pages[0];
        page.status = Some(200);
        page.content_type = "text/html".into();
        page.error = None;
        page.blocked_by_robots = false;
        page.extraction_truncated = false;
        page.robots = "noindex".into();
        let mut policy = crate::policy::AdvisoryPolicy::default();
        let (before, _) = generate(&report, &policy).unwrap();
        assert!(
            before
                .records
                .iter()
                .any(|r| r.rule_id == "noindex" && r.outcome == "not_applicable")
        );
        policy.require_indexable = true;
        let (after, _) = generate(&report, &policy).unwrap();
        assert_ne!(before.policy_digest, after.policy_digest);
        assert!(
            after
                .records
                .iter()
                .any(|r| r.rule_id == "noindex" && r.outcome == "violation")
        );
    }
    #[test]
    #[ignore = "regenerate local S14 interchange fixture"]
    fn export_candidate_fixture() {
        let (_, mut report) = sample();
        report.pages[0].status = Some(200);
        report.pages[0].content_type = "text/html".into();
        report.pages[0].title = "short".into();
        report.pages[0].error = None;
        report.pages[0].blocked_by_robots = false;
        report.pages[0].extraction_truncated = false;
        report.pages[0].links = vec![
            crate::model::Link {
                url: "https://outside.example/missing".into(),
                ..Default::default()
            },
            crate::model::Link {
                url: "https://outside.example/available".into(),
                ..Default::default()
            },
        ];
        report.pages.push(crate::model::Page {
            url: "https://outside.example/available".into(),
            status: Some(200),
            ..Default::default()
        });
        crate::policy::apply_with_evidence(&mut report, &crate::policy::AdvisoryPolicy::default())
            .unwrap();
        std::fs::write(
            "fixtures/policies/v3-local-complete.json",
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        report.pages[0].extraction_truncated = true;
        report.coverage_complete = false;
        report.schema_version = 1;
        report.policy_guidance = None;
        crate::policy::apply_with_evidence(&mut report, &crate::policy::AdvisoryPolicy::default())
            .unwrap();
        std::fs::write(
            "fixtures/policies/v3-local-partial.json",
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn resolution_requires_same_policy_complete_coverage_and_scalar_verdict() {
        let (c, _) = sample();
        let mut old = c.records[0].clone();
        old.category = "policy_violation".into();
        old.outcome = "violation".into();
        old.record_id = record_id(&old);
        let mut new = old.clone();
        new.outcome = "no_violation".into();
        assert!(resolves(&old, &new, true, true, true, true));
        assert!(!resolves(&old, &new, false, true, true, true));
        assert!(!resolves(&old, &new, true, true, true, false));
        new.outcome = "unknown".into();
        assert!(!resolves(&old, &new, true, true, true, true));
        new.outcome = "no_violation".into();
        old.discriminator = "edge:0".into();
        new.discriminator = "edge:0".into();
        assert!(!resolves(&old, &new, true, true, true, true));
    }
    #[test]
    fn canonical_unicode_and_safe_integer_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../fixtures/policies/canonical-unicode-vectors.json"
        ))
        .unwrap();
        let (_, report) = sample();
        for vector in vectors["vectors"].as_array().unwrap() {
            let candidate = Candidate {
                policy_canonical_json: vector["canonical_utf8"].as_str().unwrap().into(),
                policy_digest: vector["sha256"].as_str().unwrap().into(),
                records: vec![],
                coverage: vec![],
            };
            assert!(validate(&candidate, &report, &[]).is_ok());
        }
        for vector in vectors["negative_vectors"].as_array().unwrap() {
            let text = vector["canonical_utf8"].as_str().unwrap();
            let candidate = Candidate {
                policy_canonical_json: text.into(),
                policy_digest: format!("sha256:{}", digest(text.as_bytes())),
                records: vec![],
                coverage: vec![],
            };
            assert!(validate(&candidate, &report, &[]).is_err());
        }
    }
    #[test]
    fn validates_candidate_and_rejects_inconsistent_states() {
        let (c, r) = sample();
        let manifest = [ManifestRow {
            category: "advisory".into(),
            rule_id: "short_title".into(),
            rule_version: "s14-1".into(),
            expected_pointer: Some("/threshold".into()),
            candidates: vec![c.records[0].entity_id.clone()],
        }];
        assert!(validate(&c, &r, &manifest).is_ok());
        let mut wrong_scope = manifest.clone();
        wrong_scope[0].candidates.clear();
        assert!(validate(&c, &r, &wrong_scope).is_err());
        let mut wrong_policy = manifest.clone();
        wrong_policy[0].expected_pointer = Some("/other".into());
        assert!(validate(&c, &r, &wrong_policy).is_err());
        let mut invalid = c.clone();
        invalid.records[0].expected = Some("/missing".into());
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c.clone();
        invalid.records[0].expected = Some("/threshold~2".into());
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c.clone();
        invalid.records[0].confidence = "unknown".into();
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c.clone();
        invalid.records[0].outcome = "violation".into();
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c.clone();
        invalid.records[0].source_locator = Some("/pages/999/title".into());
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c.clone();
        invalid.coverage[0].candidates = 2;
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c.clone();
        invalid.policy_digest.push('0');
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c.clone();
        invalid.records.push(invalid.records[0].clone());
        assert!(validate(&invalid, &r, &manifest).is_err());
        let mut invalid = c;
        invalid.coverage.clear();
        assert!(validate(&invalid, &r, &manifest).is_err());
    }
}
