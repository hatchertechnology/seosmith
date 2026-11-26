//! `s14-wire-2` assembly (WP8b-3, design `wave1-wp8a.md` sections 3 and 6.1).
//!
//! `s14-wire-2` is `s14-wire-1` with exactly two changes: one new top-level
//! member, `evidence_families`, and the digest form of the S14 manifest rows.
//! [`generate_wire_v2`] runs the wire-1 pipeline under [`WireProfile::WIRE2`],
//! then budgets the families against the final bytes and appends them.
//!
//! Extensions degrade, they do not fail (A5): a family that breaks a contract
//! invariant becomes `unavailable` with `analyzer_error`, and items that do not
//! fit the byte budget are counted as omitted. Only an oversized *base* report
//! fails, with the named code `report_byte_limit`.
use crate::{
    contract::entity_id,
    delivery::HTP_MAX_REPORT_BYTES,
    families::types::{
        FAMILY_LIMITS, FamilyItem, FamilyKey, FamilySection, FamilySet, Outcome, SectionState,
        limits_for,
    },
    finding_coverage_prototype,
    generation_error::{GenerationError, JobErrorCode, ensure_code, ensure_invariant, invariant},
    model::Report,
    policy::AdvisoryPolicy,
    policy_wire_prototype::{WireProfile, no_nul},
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// The `evidence_families.semantics` string.
pub const FAMILIES_SEMANTICS: &str = "s14-families-1";
/// Cap on the whole `evidence_families` object (75 % of the 1 MiB extension cap).
pub const FAMILIES_MAX_BYTES: usize = 786_432;
/// Slack kept free between the final bytes and the HTP cap (design 3.3 step 3).
pub const REPORT_RESERVE_BYTES: usize = 65_536;
const KEY_BYTES: usize = ",\"evidence_families\":".len();
const ITEM_FACTS_MAX_BYTES: usize = 4_096;
const FACT_STRING_MAX_BYTES: usize = 2_048;
const FACTS_MAX_DEPTH: usize = 4;
const SUMMARY_MAX_BYTES: usize = 8_192;
const OMITTED_REASONS: [&str; 4] = [
    "family_item_limit",
    "family_byte_budget",
    "report_byte_budget",
    "analysis_deadline",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BudgetState {
    WithinBudget,
    ReportByteBudget,
}

impl BudgetState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::WithinBudget => "within_budget",
            Self::ReportByteBudget => "report_byte_budget",
        }
    }
}

#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    version: u32,
    semantics: &'static str,
    max_bytes: usize,
    budget_state: &'static str,
    families: &'a T,
}

fn object_len<T: Serialize>(families: &T, state: BudgetState) -> Result<usize, GenerationError> {
    let bytes = serde_json::to_vec(&Envelope {
        version: 1,
        semantics: FAMILIES_SEMANTICS,
        max_bytes: FAMILIES_MAX_BYTES,
        budget_state: state.as_str(),
        families,
    })
    .map_err(|_| invariant("evidence_families does not serialize"))?;
    Ok(bytes.len())
}

fn section_len(section: &FamilySection) -> usize {
    serde_json::to_vec(section).map_or(usize::MAX, |bytes| bytes.len())
}

/// The semantics string of a section that was never produced. The builders set
/// their own constants; this fills a key a builder left out.
const fn placeholder_semantics(key: FamilyKey) -> &'static str {
    match key {
        FamilyKey::S02 => "s02-1",
        FamilyKey::S03 => "s03-1",
        FamilyKey::S04 => "s04-1",
        FamilyKey::S05 => "s05-1",
        FamilyKey::S06 => "s06-1",
        FamilyKey::S08 => "s08-1",
    }
}

/// Six `unavailable` sections with one reason: what a job with no usable
/// capture, or no builders yet, carries. Semantics are the placeholders above.
#[must_use]
pub fn unavailable_families(reason: &str) -> FamilySet {
    FamilySet(
        FamilyKey::ALL
            .iter()
            .map(|key| {
                (
                    *key,
                    FamilySection::unavailable(*key, placeholder_semantics(*key), reason),
                )
            })
            .collect(),
    )
}

// --------------------------------------------------------------- invariants

fn code_ok(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn reason_ok(reason: Option<&String>) -> bool {
    reason.is_none_or(|r| code_ok(r))
}

fn strings_bounded(value: &Value, depth: usize) -> bool {
    match value {
        Value::String(s) => s.len() <= FACT_STRING_MAX_BYTES && !s.contains('\0'),
        Value::Array(items) => {
            depth < FACTS_MAX_DEPTH && items.iter().all(|v| strings_bounded(v, depth + 1))
        }
        Value::Object(map) => {
            depth < FACTS_MAX_DEPTH
                && map
                    .iter()
                    .all(|(k, v)| !k.contains('\0') && strings_bounded(v, depth + 1))
        }
        _ => true,
    }
}

/// `/pages/N`, `/pages/N/links/M` or `/pages/N/relationships/M`, resolved in `report`.
fn locator_page<'a>(report: &'a Report, locator: &str) -> Option<&'a crate::model::Page> {
    let rest = locator.strip_prefix("/pages/")?;
    let canonical = |s: &str| s.parse::<usize>().ok().filter(|n| n.to_string() == s);
    let (page, tail) = match rest.split_once('/') {
        Some((p, t)) => (p, Some(t)),
        None => (rest, None),
    };
    let page = report.pages.get(canonical(page)?)?;
    match tail {
        None => Some(page),
        Some(t) => {
            let (kind, index) = t.split_once('/')?;
            let index = canonical(index)?;
            let len = match kind {
                "links" => page.links.len(),
                "relationships" => page.relationships.len(),
                _ => return None,
            };
            (index < len).then_some(page)
        }
    }
}

fn check_item(report: &Report, item: &FamilyItem) -> Result<(), &'static str> {
    if !code_ok(&item.kind) || !reason_ok(item.reason.as_ref()) {
        return Err("item code");
    }
    if item.outcome == Outcome::Unknown && item.reason.is_none() {
        return Err("unknown item without a reason");
    }
    if item.outcome == Outcome::Pass
        && matches!(
            item.reason.as_deref(),
            Some("collector_byte_limit" | "run_byte_limit")
        )
    {
        return Err("capture omission read as pass");
    }
    match (&item.locator, &item.entity_id) {
        (Some(locator), Some(id)) => {
            let page = locator_page(report, locator).ok_or("unresolved locator")?;
            if *id != entity_id(&page.url, page.is_resource) {
                return Err("entity does not match the locator");
            }
        }
        (None, None) => {
            if !item.facts.get("url").is_some_and(Value::is_string) {
                return Err("unbound item without facts.url");
            }
        }
        _ => return Err("locator and entity must be both set or both null"),
    }
    let facts = Value::Object(item.facts.clone());
    if !strings_bounded(&facts, 0) {
        return Err("facts shape");
    }
    if serde_json::to_vec(&facts).map_or(true, |b| b.len() > ITEM_FACTS_MAX_BYTES) {
        return Err("facts size");
    }
    Ok(())
}

/// Every section invariant of design 3.2 that the producer can break.
fn check_section(report: &Report, section: &FamilySection) -> Result<(), &'static str> {
    if section.section_version != 1 || section.semantics.is_empty() || section.semantics.len() > 64
    {
        return Err("section header");
    }
    if !reason_ok(section.reason.as_ref()) || !reason_ok(section.omitted_reason.as_ref()) {
        return Err("section code");
    }
    let c = &section.coverage;
    if c.unknown_reasons.values().sum::<u64>() != c.unknown
        || !c.unknown_reasons.keys().all(|k| code_ok(k))
    {
        return Err("coverage unknown_reasons");
    }
    let omitted = section.omitted_items;
    if section.items_total != section.items.len() as u64 + omitted {
        return Err("items_total");
    }
    if section.truncated != (omitted > 0) {
        return Err("truncated");
    }
    match (&section.omitted_reason, section.truncated) {
        (Some(r), true) if OMITTED_REASONS.contains(&r.as_str()) => {}
        (None, false) => {}
        _ => return Err("omitted_reason"),
    }
    match section.state {
        SectionState::Complete => {
            if c.unknown != 0 || section.truncated {
                return Err("complete section with unknowns or omissions");
            }
        }
        SectionState::Partial => {}
        SectionState::Unavailable => {
            if !section.items.is_empty() || section.reason.is_none() || section.truncated {
                return Err("unavailable section shape");
            }
        }
    }
    let summary_ok = serde_json::to_vec(&section.summary)
        .is_ok_and(|b| b.len() <= SUMMARY_MAX_BYTES)
        && no_nul(&section.summary);
    if !summary_ok {
        return Err("summary");
    }
    for item in &section.items {
        check_item(report, item)?;
    }
    Ok(())
}

fn rank(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Issue => 0,
        Outcome::Unknown => 1,
        Outcome::Observed => 2,
        Outcome::Pass => 3,
    }
}

fn degraded(key: FamilyKey, section: &FamilySection, reason: &str) -> FamilySection {
    let semantics = if section.semantics.is_empty() || section.semantics.len() > 64 {
        placeholder_semantics(key)
    } else {
        section.semantics.as_str()
    };
    FamilySection::unavailable(key, semantics, reason)
}

// ------------------------------------------------------------------- budget

/// How many bytes `evidence_families` may take, decided from the base report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    /// Cap on the whole `evidence_families` object.
    budget: usize,
    state: BudgetState,
}

/// `base` is the report without `evidence_families`; `skeleton` is the object
/// holding six `unavailable` sections. The whole report must leave
/// [`REPORT_RESERVE_BYTES`] free under `cap`.
fn plan(base: usize, skeleton: usize, cap: usize) -> Result<Plan, GenerationError> {
    let fixed = base
        .saturating_add(KEY_BYTES)
        .saturating_add(REPORT_RESERVE_BYTES);
    ensure_code(
        fixed.saturating_add(skeleton) <= cap,
        JobErrorCode::ReportByteLimit,
        "report leaves no room for evidence families",
    )?;
    let room = cap - fixed;
    Ok(if room < FAMILIES_MAX_BYTES {
        Plan {
            budget: room,
            state: BudgetState::ReportByteBudget,
        }
    } else {
        Plan {
            budget: FAMILIES_MAX_BYTES,
            state: BudgetState::WithinBudget,
        }
    })
}

/// Fits one validated section into `allowance` bytes (and its own item cap).
/// The reason for a new omission is `family_item_limit` when the item cap bound,
/// else the byte-budget reason; a reason the builder already set is kept.
fn fit(key: FamilyKey, mut section: FamilySection, allowance: usize, plan: Plan) -> FamilySection {
    let limits = limits_for(key);
    section.max_items = limits.max_items as u64;
    section.max_bytes = limits.max_bytes as u64;
    if section.state == SectionState::Unavailable {
        return section;
    }
    // Deterministic order (design 3.2-7); the sort is stable.
    section.items.sort_by(|a, b| {
        (rank(a.outcome), &a.locator, &a.kind).cmp(&(rank(b.outcome), &b.locator, &b.kind))
    });
    let allowance = allowance.min(limits.max_bytes);
    let builder_omitted = section.omitted_items;
    let builder_reason = section.omitted_reason.clone();
    let byte_reason = match plan.state {
        BudgetState::ReportByteBudget => "report_byte_budget",
        BudgetState::WithinBudget => "family_byte_budget",
    };
    let full = std::mem::take(&mut section.items);
    let item_cap = full.len().min(limits.max_items);
    let build = |keep: usize| -> FamilySection {
        let mut s = section.clone();
        s.items = full[..keep].to_vec();
        s.omitted_items = builder_omitted + (full.len() - keep) as u64;
        s.items_total = keep as u64 + s.omitted_items;
        s.truncated = s.omitted_items > 0;
        s.omitted_reason = s.truncated.then(|| {
            builder_reason.clone().unwrap_or_else(|| {
                if keep == item_cap && keep < full.len() {
                    "family_item_limit".to_owned()
                } else {
                    byte_reason.to_owned()
                }
            })
        });
        if s.truncated && s.state == SectionState::Complete {
            s.state = SectionState::Partial;
        }
        s
    };
    // Estimate how many items fit from their sizes, then settle exactly.
    let fixed = section_len(&build(0));
    let mut used = fixed;
    let mut keep = 0;
    for item in &full[..item_cap] {
        let size = serde_json::to_vec(item).map_or(usize::MAX, |b| b.len());
        let next = used
            .saturating_add(size)
            .saturating_add(usize::from(keep > 0));
        if next > allowance {
            break;
        }
        used = next;
        keep += 1;
    }
    let mut out = build(keep);
    // The omission fields can grow by a few bytes; drop items until it fits.
    while section_len(&out) > allowance && keep > 0 {
        keep -= 1;
        out = build(keep);
    }
    out
}

/// Normalizes and fits all six sections. Never fails: a broken section is
/// replaced by an `unavailable` one with `analyzer_error`.
fn assemble(
    report: &Report,
    mut families: FamilySet,
    plan: Plan,
) -> Result<FamilySet, GenerationError> {
    let mut checked = BTreeMap::new();
    for key in FamilyKey::ALL {
        let section = families.0.remove(&key).unwrap_or_else(|| {
            FamilySection::unavailable(key, placeholder_semantics(key), "analyzer_error")
        });
        let section = match check_section(report, &section) {
            Ok(()) => section,
            Err(_) => degraded(key, &section, "analyzer_error"),
        };
        checked.insert(key, section);
    }
    // Wrapper bytes: the object with six `null` sections.
    let nulls: BTreeMap<FamilyKey, Option<u8>> =
        FamilyKey::ALL.iter().map(|k| (*k, None)).collect();
    let overhead = object_len(&nulls, plan.state)?.saturating_sub(6 * 4);
    let total = plan.budget.saturating_sub(overhead);
    let max_sum: usize = FAMILY_LIMITS.iter().map(|(_, l)| l.max_bytes).sum();
    let mut fitted = BTreeMap::new();
    for (key, section) in checked {
        let share = total * limits_for(key).max_bytes / max_sum;
        fitted.insert(key, fit(key, section, share, plan));
    }
    let mut set = FamilySet(fitted);
    // Per-family shares can undershoot an `unavailable` section when the report
    // is nearly full; give up the largest sections until the object fits. The
    // all-`unavailable` skeleton fits by `plan`, so this terminates.
    while object_len(&set.0, plan.state)? > plan.budget {
        let largest = set
            .0
            .iter()
            .filter(|(_, s)| s.state != SectionState::Unavailable)
            .max_by_key(|(_, s)| section_len(s))
            .map(|(k, s)| (*k, s.semantics.clone()));
        let Some((key, semantics)) = largest else {
            break;
        };
        set.0.insert(
            key,
            FamilySection::unavailable(key, &semantics, "report_byte_budget"),
        );
    }
    ensure_invariant!(
        object_len(&set.0, plan.state)? <= plan.budget,
        "evidence families exceed their budget"
    );
    for key in FamilyKey::ALL {
        let section = set.0.get(&key).ok_or_else(|| invariant("family missing"))?;
        ensure_invariant!(
            section.items.len() <= limits_for(key).max_items
                && check_section(report, section).is_ok(),
            "assembled family violates the contract"
        );
    }
    Ok(set)
}

// ------------------------------------------------------------------ the API

/// Serializes a wire-2 report: the wire-1 pipeline under [`WireProfile::WIRE2`]
/// (S01 limits 1,024/1,024, digest manifest rows, label `s14-wire-2`), then
/// `evidence_families` budgeted against the final bytes. The returned bytes are
/// what is stored and delivered, byte for byte.
pub fn generate_wire_v2(
    report: &mut Report,
    policy: &AdvisoryPolicy,
    families: FamilySet,
) -> Result<Vec<u8>, GenerationError> {
    generate_wire_v2_with_cap(report, policy, families, HTP_MAX_REPORT_BYTES)
}

/// [`generate_wire_v2`] under an explicit report byte cap (a test seam; the
/// production cap is `HTP_MAX_REPORT_BYTES`).
pub fn generate_wire_v2_with_cap(
    report: &mut Report,
    policy: &AdvisoryPolicy,
    families: FamilySet,
    cap: usize,
) -> Result<Vec<u8>, GenerationError> {
    let base_bytes =
        finding_coverage_prototype::generate_wire_capped(report, policy, WireProfile::WIRE2, cap)?;
    let base = base_bytes.len();
    // The skeleton uses the longest reason and the real semantics strings.
    let skeleton: BTreeMap<FamilyKey, FamilySection> = FamilyKey::ALL
        .iter()
        .map(|key| {
            let semantics = families
                .0
                .get(key)
                .map_or(placeholder_semantics(*key), |s| s.semantics.as_str());
            let semantics = if semantics.is_empty() || semantics.len() > 64 {
                placeholder_semantics(*key)
            } else {
                semantics
            };
            (
                *key,
                FamilySection::unavailable(*key, semantics, "report_byte_budget"),
            )
        })
        .collect();
    let plan = plan(
        base,
        object_len(&skeleton, BudgetState::ReportByteBudget)?,
        cap,
    )?;
    let set = assemble(report, families, plan)?;
    let mut value: Value = serde_json::from_slice(&base_bytes)
        .map_err(|_| invariant("base report is not valid JSON"))?;
    let object = serde_json::to_value(Envelope {
        version: 1,
        semantics: FAMILIES_SEMANTICS,
        max_bytes: FAMILIES_MAX_BYTES,
        budget_state: plan.state.as_str(),
        families: &set,
    })
    .map_err(|_| invariant("evidence_families does not serialize"))?;
    value
        .as_object_mut()
        .ok_or_else(|| invariant("report is not an object"))?
        .insert("evidence_families".into(), object);
    let bytes =
        serde_json::to_vec(&value).map_err(|_| invariant("wire-2 report does not serialize"))?;
    ensure_code(
        bytes.len() <= cap,
        JobErrorCode::ReportByteLimit,
        "wire-2 report exceeds the report byte limit",
    )?;
    ensure_invariant!(
        bytes.len() <= base + KEY_BYTES + plan.budget,
        "evidence families exceed their budget"
    );
    persistable(&bytes, &report.request.request_id.to_string())?;
    Ok(bytes)
}

/// The persistability guard. It runs before `Store::finish_bytes`, which
/// enforces the same conditions, so `ReportTooLarge` and the header checks
/// there are unreachable from wire-2: a failure here is a *named generation
/// error*, not an untyped persistence error.
fn persistable(bytes: &[u8], request_id: &str) -> Result<(), GenerationError> {
    ensure_code(
        bytes.len() <= HTP_MAX_REPORT_BYTES,
        JobErrorCode::ReportByteLimit,
        "report exceeds the persistable byte limit",
    )?;
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| invariant("report is not valid JSON"))?;
    ensure_invariant!(
        value.get("schema_version").and_then(Value::as_u64) == Some(4)
            && value.pointer("/request/request_id").and_then(Value::as_str) == Some(request_id),
        "report is not the schema-4 bytes of this job"
    );
    ensure_invariant!(
        value
            .pointer("/evidence_contract/version")
            .and_then(Value::as_u64)
            == Some(1)
            && value.get("finding_coverage").is_some_and(Value::is_object),
        "report lacks the extensions the store requires"
    );
    ensure_invariant!(no_nul(&value), "NUL is unsupported on the receiver");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_fails_with_the_named_code_one_byte_over() {
        let skeleton = 1_000;
        let cap = 4_952_756;
        // Exactly fits: base + key + skeleton + reserve == cap.
        let base = cap - REPORT_RESERVE_BYTES - KEY_BYTES - skeleton;
        let p = plan(base, skeleton, cap).unwrap();
        assert_eq!(p.budget, skeleton);
        assert_eq!(p.state, BudgetState::ReportByteBudget);
        // One byte over.
        let e = plan(base + 1, skeleton, cap).unwrap_err();
        assert_eq!(e.code, JobErrorCode::ReportByteLimit);
        // One byte under: one spare byte of budget.
        assert_eq!(plan(base - 1, skeleton, cap).unwrap().budget, skeleton + 1);
    }

    #[test]
    fn plan_is_within_budget_exactly_when_the_room_covers_the_cap() {
        let cap = 4_952_756;
        let skeleton = 1_000;
        let base = cap - REPORT_RESERVE_BYTES - KEY_BYTES - FAMILIES_MAX_BYTES;
        let p = plan(base, skeleton, cap).unwrap();
        assert_eq!(p.budget, FAMILIES_MAX_BYTES);
        assert_eq!(p.state, BudgetState::WithinBudget);
        let p = plan(base + 1, skeleton, cap).unwrap();
        assert_eq!(p.budget, FAMILIES_MAX_BYTES - 1);
        assert_eq!(p.state, BudgetState::ReportByteBudget);
        let p = plan(base - 1, skeleton, cap).unwrap();
        assert_eq!(p.budget, FAMILIES_MAX_BYTES);
        assert_eq!(p.state, BudgetState::WithinBudget);
    }

    #[test]
    fn the_guard_names_each_failure() {
        let ok = serde_json::json!({
            "schema_version": 4,
            "request": {"request_id": "r1"},
            "evidence_contract": {"version": 1},
            "finding_coverage": {}
        });
        let bytes = serde_json::to_vec(&ok).unwrap();
        assert!(persistable(&bytes, "r1").is_ok());
        assert_eq!(
            persistable(&bytes, "r2").unwrap_err().code,
            JobErrorCode::GenerationInvariant
        );
        let mut wrong = ok.clone();
        wrong["schema_version"] = 3.into();
        assert!(persistable(&serde_json::to_vec(&wrong).unwrap(), "r1").is_err());
        let mut nul = ok.clone();
        nul["x"] = "a\0b".into();
        assert!(persistable(&serde_json::to_vec(&nul).unwrap(), "r1").is_err());
        let mut no_ec = ok.clone();
        no_ec.as_object_mut().unwrap().remove("evidence_contract");
        assert!(persistable(&serde_json::to_vec(&no_ec).unwrap(), "r1").is_err());
        let mut big = serde_json::to_vec(&ok).unwrap();
        big.resize(HTP_MAX_REPORT_BYTES + 1, b' ');
        assert_eq!(
            persistable(&big, "r1").unwrap_err().code,
            JobErrorCode::ReportByteLimit
        );
        big.truncate(HTP_MAX_REPORT_BYTES);
        assert!(persistable(&big, "r1").is_ok());
    }
}
