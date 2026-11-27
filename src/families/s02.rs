//! S02 indexing and crawler permissions, from `saved_index_integrity` findings.
use super::{
    common::{Ctx, SectionBuilder, capture_omissions, facts, slug},
    types::{FamilyKey, FamilySection, Outcome},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const SEMANTICS: &str = "s02-indexing-1";

const S02_RULES: [&str; 10] = [
    "page_blocked_by_robots",
    "page_redirect",
    "page_http_error",
    "page_evidence_unavailable",
    "crawl_denied",
    "crawl_unknown",
    "directive_conflict",
    "index_allowed",
    "index_restricted",
    "index_unknown",
];

pub fn is_s02_rule(rule: &str) -> bool {
    S02_RULES.contains(&rule)
}

pub(crate) fn outcome_of(rule: &str, kind: &str) -> Outcome {
    match (kind, rule) {
        ("negative", _) => Outcome::Issue,
        ("uncertain", _) => Outcome::Unknown,
        (_, "index_allowed") => Outcome::Pass,
        _ => Outcome::Observed,
    }
}

/// Reason text of a finding: a string, or a structured value reduced to its JSON.
pub(crate) fn reason_text(finding: &Value) -> String {
    match &finding["reason"] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// One item per (page, rule, kind, reason); the bots that share it go in `facts.bots`.
pub(crate) fn push_findings(
    ctx: &Ctx<'_>,
    builder: &mut SectionBuilder,
    findings: &[Value],
    keep: impl Fn(&str) -> bool,
) -> (u64, std::collections::HashSet<usize>) {
    type Key = (String, String, String, String);
    let mut groups: BTreeMap<Key, (Vec<String>, Value)> = BTreeMap::new();
    for f in findings {
        let (Some(rule), Some(kind), Some(url)) =
            (f["rule"].as_str(), f["kind"].as_str(), f["url"].as_str())
        else {
            continue;
        };
        if !keep(rule) {
            continue;
        }
        let entry = groups
            .entry((
                url.to_owned(),
                rule.to_owned(),
                kind.to_owned(),
                reason_text(f),
            ))
            .or_insert_with(|| (Vec::new(), f.clone()));
        if let Some(bot) = f["bot"].as_str()
            && !entry.0.contains(&bot.to_owned())
        {
            entry.0.push(bot.to_owned());
        }
    }
    let mut decided = std::collections::HashSet::new();
    let mut pushed = 0u64;
    for ((url, rule, kind, reason), (bots, sample)) in groups {
        let Some(entity) = ctx.entity(&url) else {
            continue;
        };
        let outcome = outcome_of(&rule, &kind);
        if outcome != Outcome::Unknown {
            decided.insert(ctx.by_url[&url]);
        }
        let mut f = facts(&[
            ("rule", json!(rule)),
            ("detail", json!(reason)),
            ("bots", json!(bots)),
        ]);
        if let Some(path) = sample["path"].as_array() {
            f.insert("path".into(), json!(path));
        }
        if let Some(source) = sample["source"].as_object() {
            f.insert("source".into(), Value::Object(source.clone()));
        }
        let code = if outcome == Outcome::Unknown {
            Some(slug(&reason))
        } else {
            None
        };
        builder.push(Some(entity), &rule, outcome, code.as_deref(), f);
        pushed += 1;
    }
    (pushed, decided)
}

pub fn build(ctx: &Ctx<'_>, index: &Value) -> FamilySection {
    let eligible = ctx.entities.iter().filter(|e| e.html).count() as u64;
    let mut b = SectionBuilder::new(FamilyKey::S02, SEMANTICS, ctx, eligible);
    let findings = index["findings"].as_array().map_or(&[][..], Vec::as_slice);
    let (_, decided) = push_findings(ctx, &mut b, findings, is_s02_rule);
    let omitted = capture_omissions(
        ctx,
        &mut b,
        &["html_occurrences", "effective_indexing"],
        true,
    );
    let attempted = ctx
        .entities
        .iter()
        .filter(|e| e.html && ctx.captures.contains_key(&e.url))
        .count() as u64;
    b.attempted(attempted);
    b.observed(decided.iter().filter(|n| !omitted.contains(n)).count() as u64);
    if index["coverage"]["row_limit_reached"] == true {
        b.partial("analyzer_row_limit");
    }
    b.summary(json!({
        "bots": index["bots"],
        "analyzer_state": index["coverage"]["state"],
        "rows_seen": index["coverage"]["rows_seen"],
        "robots": index["robots"]["state"],
    }));
    b.finish()
}
