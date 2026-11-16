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
                && report
                    .pages
                    .iter()
                    .any(|p| contract::entity_id(&p.url, p.is_resource) == *id)),
            "invalid candidate universe"
        );
    }
    let matrix: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/policies/state-matrix.json"))?;
    let tuples = matrix["tuples"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid matrix"))?;
    let mut ids = HashSet::new();
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
                && rule.candidates.contains(&r.entity_id)
                && r.expected == rule.expected_pointer,
            "rule or scope mismatch"
        );
        ensure!(r.discriminator == "self", "edge scope not implemented");
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
        if let Some(pointer) = &r.source_locator {
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
            let original = serde_json::to_value(report)?;
            ensure!(original.pointer(pointer).is_some(), "source missing");
            ensure!(
                r.discriminator == "self"
                    || (parts.len() >= 5
                        && parts[3] == "relationships"
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
                eligibility_counts: BTreeMap::from([("eligible".into(), 1)]),
                attempt_counts: BTreeMap::from([("completed".into(), 1)]),
                outcomes: BTreeMap::from([("advisory_triggered".into(), 1)]),
                unknown_reasons: BTreeMap::new(),
            }],
        };
        (c, report)
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
