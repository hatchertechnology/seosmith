//! Schema-4 (`s14-wire-1`) report generation.
//!
//! [`generate_wire_v1`] is the production serialiser: it attaches S01 evidence
//! over the final findings. [`generate`] is the evidence-free fixture form kept
//! for the pinned review fixtures and local tools. Neither is a delivery gate;
//! the receiver independently recomputes the population.
use crate::{
    delivery::HTP_MAX_REPORT_BYTES,
    model::{Report, digest},
    policy::{self, AdvisoryPolicy},
};
// TEMPORARY (S1): switch to `crate::contract::attach_v1_schema4` after E-p lands.
use crate::contract_stub_ep::attach_v1_schema4;
use anyhow::{Result, ensure};
use std::collections::{HashMap, HashSet};

/// Maximum serialised size of one top-level extension (`MAX_EXTENSION_BYTES`).
const MAX_EXTENSION_BYTES: usize = 1_048_576;

fn no_nul(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => !s.contains('\0'),
        serde_json::Value::Array(a) => a.iter().all(no_nul),
        serde_json::Value::Object(o) => o.iter().all(|(k, v)| !k.contains('\0') && no_nul(v)),
        _ => true,
    }
}

// Schema 3 is frozen. Recompute wire edge states from source evidence, including omitted edges.
fn reconcile_wire_edges(report: &mut Report) -> Result<()> {
    let targets: HashMap<_, _> = report
        .pages
        .iter()
        .enumerate()
        .map(|(i, page)| (page.url.as_str(), (i, page)))
        .collect();
    let mut edges = Vec::new();
    let mut all_sources_usable = true;
    for (page_index, page) in report.pages.iter().enumerate() {
        if page.is_resource
            || !url::Url::parse(&page.url).is_ok_and(|u| report.request.in_scope(&u))
        {
            continue;
        }
        let usable = !page.extraction_truncated
            && !page.blocked_by_robots
            && page.error.is_none()
            && page.html();
        all_sources_usable &= usable;
        for (link_index, link) in page.links.iter().enumerate() {
            if !url::Url::parse(&link.url).is_ok_and(|u| !report.request.in_scope(&u)) {
                continue;
            }
            let target = targets.get(link.url.as_str()).copied();
            let reason = if !usable {
                Some("source_unavailable")
            } else if target.is_some_and(|(_, p)| {
                p.status.is_none() || p.error.is_some() || p.blocked_by_robots
            }) {
                Some("external_target_unavailable")
            } else if target.is_none() && report.request.check_external_links {
                Some("external_target_not_fetched")
            } else if target.is_none() {
                Some("external_check_disabled")
            } else {
                None
            };
            edges.push((
                format!("/pages/{page_index}/links/{link_index}"),
                target.map(|(i, p)| (i, p.status)),
                reason,
            ));
        }
    }
    let candidate = report
        .policy_guidance
        .as_mut()
        .and_then(|g| g.candidate.as_mut())
        .ok_or_else(|| anyhow::anyhow!("candidate missing"))?;
    let indexes: HashMap<_, _> = candidate
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.category == "observation" && r.rule_id == "external_link")
        .filter_map(|(i, r)| r.context_locator.clone().map(|p| (p, i)))
        .collect();
    let coverage = candidate
        .coverage
        .iter_mut()
        .find(|c| c.category == "observation" && c.rule_id == "external_link")
        .ok_or_else(|| anyhow::anyhow!("edge coverage missing"))?;
    ensure!(
        coverage.candidates == edges.len(),
        "edge population differs"
    );
    coverage.eligibility_counts.clear();
    coverage.attempt_counts.clear();
    coverage.outcomes.clear();
    coverage.unknown_reasons.clear();
    let mut emitted = 0;
    for (pointer, target, reason) in edges {
        let (eligibility, attempt, outcome) = if reason.is_some() {
            ("unknown", "not_attempted", "unknown")
        } else {
            ("eligible", "completed", "observed")
        };
        *coverage
            .eligibility_counts
            .entry(eligibility.into())
            .or_default() += 1;
        *coverage.attempt_counts.entry(attempt.into()).or_default() += 1;
        *coverage.outcomes.entry(outcome.into()).or_default() += 1;
        if let Some(reason) = reason {
            *coverage.unknown_reasons.entry(reason.into()).or_default() += 1;
        }
        if let Some(index) = indexes.get(&pointer) {
            emitted += 1;
            let record = &mut candidate.records[*index];
            record.eligibility = eligibility.into();
            record.attempt = attempt.into();
            record.outcome = outcome.into();
            record.reason = reason.map(str::to_owned);
            if reason.is_some() {
                record.actual = "unavailable".into();
                record.observed_fact = None;
                record.source_locator = None;
                record.target_locator = None;
                record.confidence = "unknown".into();
            } else {
                let (target_index, status) =
                    target.ok_or_else(|| anyhow::anyhow!("observed edge has no target"))?;
                record.actual = "source".into();
                record.observed_fact = Some(serde_json::json!(
                    status.ok_or_else(|| anyhow::anyhow!("target status missing"))?
                ));
                record.source_locator = Some(pointer);
                record.target_locator = Some(format!("/pages/{target_index}/status"));
                record.confidence = "high".into();
            }
        }
    }
    ensure!(
        emitted == coverage.emitted && coverage.emitted + coverage.omitted == coverage.candidates,
        "edge emission mismatch"
    );
    coverage.complete = report.coverage_complete
        && report.request.check_external_links
        && all_sources_usable
        && coverage.omitted == 0
        && coverage.outcomes.get("unknown").copied().unwrap_or(0) == 0;
    Ok(())
}

/// Evidence-free schema-4 form, used by the pinned review fixtures and local
/// tools. The production path is [`generate_wire_v1`].
pub fn generate(report: &mut Report, config: &AdvisoryPolicy) -> Result<Vec<u8>> {
    generate_with(report, config, false)
}

/// Production schema-4 serialisation: `s14-wire-1` plus S01 evidence v1 over the
/// final (post-policy) findings. The returned bytes are what is stored and
/// delivered, byte for byte.
pub fn generate_wire_v1(report: &mut Report, config: &AdvisoryPolicy) -> Result<Vec<u8>> {
    generate_with(report, config, true)
}

fn generate_with(report: &mut Report, config: &AdvisoryPolicy, evidence: bool) -> Result<Vec<u8>> {
    ensure!(
        report.schema_version == 1
            && report.evidence_contract.is_none()
            && report.policy_guidance.is_none(),
        "prototype requires an unversioned base report"
    );
    policy::apply_with_evidence(report, config)?;
    let guidance = report
        .policy_guidance
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("policy missing"))?;
    let candidate = guidance
        .candidate
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("candidate missing"))?;
    let manifest = guidance
        .manifest
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("manifest missing"))?;
    crate::policy_candidate::validate_generated(candidate, manifest, report, config)?;
    let mut entities = HashSet::new();
    let mut external_urls = HashSet::new();
    for page in &report.pages {
        ensure!(
            entities.insert((page.url.as_str(), page.is_resource)),
            "duplicate source entity"
        );
        let url = url::Url::parse(&page.url)?;
        if !report.request.in_scope(&url) {
            ensure!(
                external_urls.insert(page.url.as_str()),
                "ambiguous external target URL"
            );
        }
    }
    reconcile_wire_edges(report)?;
    if evidence {
        // `report.findings` is final here; evidence must not run earlier.
        // `apply_with_evidence` leaves the local schema number (3) on the report;
        // `attach_v1_schema4` requires the unversioned 1 and never sets a version.
        // The schema is set to 4 below.
        report.schema_version = 1;
        attach_v1_schema4(report)?;
        ensure!(
            report.schema_version == 1,
            "evidence attachment must not set the schema version"
        );
        ensure!(
            serde_json::to_vec(&report.evidence_contract)?.len() <= MAX_EXTENSION_BYTES,
            "evidence extension exceeds byte limit"
        );
    }
    let policy = serde_json::json!({"catalog_version":"s14-1","collection":{"check_external_links":report.request.check_external_links,"delay_ms":report.request.delay_ms,"max_duration_seconds":report.request.max_duration_seconds,"max_pages":report.request.max_pages,"max_resources":report.request.max_resources,"seed_url":report.request.url},"engine_version":report.engine_version,"policy":config,"ruleset_version":report.ruleset_version,"schema_version":4,"validator_version":"s14-wire-1","wire_contract_version":"s14-wire-1"});
    // This typed object has a fixed two-container shape; it is not an untrusted JSON parser.
    let canonical = serde_json::to_string(&policy)?;
    ensure!(
        canonical.len() <= 65_536,
        "prototype policy exceeds byte limit"
    );
    let signature = format!("sha256:{}", digest(canonical.as_bytes()));
    let guidance = report
        .policy_guidance
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("policy missing"))?;
    guidance.version = "s14-wire-1".into();
    let candidate = guidance
        .candidate
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("candidate missing"))?;
    candidate.policy_canonical_json = canonical;
    candidate.policy_digest.clone_from(&signature);
    for record in &mut candidate.records {
        record.policy_ref.clone_from(&signature);
    }
    report.schema_version = 4;
    ensure!(
        serde_json::to_vec(&report.policy_guidance)?.len() <= 1_048_576,
        "prototype policy extension exceeds byte limit"
    );
    let bytes = serde_json::to_vec(report)?;
    ensure!(
        bytes.len() <= HTP_MAX_REPORT_BYTES,
        "prototype exceeds exact report byte limit"
    );
    ensure!(
        no_nul(&serde_json::to_value(report)?),
        "NUL is unsupported on the receiver"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_records_match_provisional_fixture_values() -> Result<()> {
        let fixtures = [
            (
                include_str!("../fixtures/policies/v3-local-complete.json"),
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-complete.json"
                ),
            ),
            (
                include_str!("../fixtures/policies/v3-local-partial.json"),
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-partial.json"
                ),
            ),
            (
                include_str!("../fixtures/policies/v3-local-omitted.json"),
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-omitted.json"
                ),
            ),
            (
                include_str!("../fixtures/policies/v3-local-intent.json"),
                include_str!("../fixtures/contracts/s14-wire-1-candidate/v4-candidate-intent.json"),
            ),
            (
                include_str!("../fixtures/policies/v3-local-blocked.json"),
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-blocked.json"
                ),
            ),
            (
                include_str!("../fixtures/policies/v3-local-reordered.json"),
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-reordered.json"
                ),
            ),
            (
                include_str!("../fixtures/policies/v3-local-resolved.json"),
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-resolved.json"
                ),
            ),
        ];
        for (local, wire) in fixtures {
            let mut report: Report = serde_json::from_str(local)?;
            let config = report.policy_guidance.take().unwrap().config;
            report.schema_version = 1;
            let actual = generate(&mut report, &config)?;
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&actual)?,
                serde_json::from_str::<serde_json::Value>(wire)?
            );
            ensure!(
                actual == wire.as_bytes(),
                "producer serialization differs from pinned fixture bytes"
            );
        }
        Ok(())
    }
    #[test]
    fn wire_edges_preserve_unknown_source_and_target_states() -> Result<()> {
        let cases = [
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-control.json"
                ),
                "observed",
                None,
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-source_missing_status.json"
                ),
                "unknown",
                Some("source_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-source_non_html.json"
                ),
                "unknown",
                Some("source_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-source_non_success.json"
                ),
                "unknown",
                Some("source_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-source_error.json"
                ),
                "unknown",
                Some("source_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-source_blocked.json"
                ),
                "unknown",
                Some("source_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-source_truncated.json"
                ),
                "unknown",
                Some("source_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-target_missing_status.json"
                ),
                "unknown",
                Some("external_target_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-target_error.json"
                ),
                "unknown",
                Some("external_target_unavailable"),
            ),
            (
                include_str!(
                    "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-target_blocked.json"
                ),
                "unknown",
                Some("external_target_unavailable"),
            ),
        ];
        for (wire, outcome, reason) in cases {
            let mut report: Report = serde_json::from_str(wire)?;
            let config = report.policy_guidance.take().unwrap().config;
            report.schema_version = 1;
            report.findings.clear();
            let bytes = generate(&mut report, &config)?;
            ensure!(bytes == wire.as_bytes(), "edge fixture bytes changed");
            let candidate = report
                .policy_guidance
                .as_ref()
                .unwrap()
                .candidate
                .as_ref()
                .unwrap();
            let edge = candidate
                .records
                .iter()
                .find(|r| {
                    r.rule_id == "external_link"
                        && r.context_locator.as_deref() == Some("/pages/0/links/1")
                })
                .unwrap();
            assert_eq!(edge.outcome, outcome);
            assert_eq!(edge.reason.as_deref(), reason);
        }
        Ok(())
    }

    #[test]
    fn wire_recomputes_omitted_edge_aggregates() -> Result<()> {
        let wire = include_str!(
            "../fixtures/contracts/s14-wire-1-candidate/v4-candidate-edge-omitted.json"
        );
        let mut report: Report = serde_json::from_str(wire)?;
        let config = report.policy_guidance.take().unwrap().config;
        report.schema_version = 1;
        report.findings.clear();
        ensure!(
            generate(&mut report, &config)? == wire.as_bytes(),
            "omitted fixture bytes changed"
        );
        let candidate = report
            .policy_guidance
            .as_ref()
            .unwrap()
            .candidate
            .as_ref()
            .unwrap();
        let coverage = candidate
            .coverage
            .iter()
            .find(|r| r.rule_id == "external_link")
            .unwrap();
        assert_eq!(coverage.omitted, 112);
        assert_eq!(coverage.outcomes.get("observed"), Some(&19));
        assert_eq!(coverage.outcomes.get("unknown"), Some(&93));
        assert_eq!(
            coverage.unknown_reasons.get("source_unavailable"),
            Some(&74)
        );
        assert_eq!(
            coverage.unknown_reasons.get("external_target_not_fetched"),
            Some(&19)
        );
        assert!(!coverage.complete);
        Ok(())
    }

    #[test]
    fn ambiguous_external_target_is_rejected() -> Result<()> {
        let mut report: Report = serde_json::from_str(include_str!(
            "../fixtures/contracts/s14-wire-1-candidate/invalid-ambiguous-external-target.json"
        ))?;
        let config = report.policy_guidance.take().unwrap().config;
        report.schema_version = 1;
        report.findings.clear();
        assert!(generate(&mut report, &config).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn prototype_cannot_enter_the_delivery_store() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("test.sqlite");
        let store = crate::store::Store::open(db.to_str().unwrap()).await?;
        let mut report: Report =
            serde_json::from_str(include_str!("../fixtures/policies/v3-local-complete.json"))?;
        let config = report.policy_guidance.take().unwrap().config;
        report.schema_version = 1;
        store.submit(&report.request).await?;
        store.claim().await?.unwrap();
        generate(&mut report, &config)?;
        assert!(store.finish(&report).await.is_err());
        assert!(store.pending().await?.is_none());
        Ok(())
    }
}
