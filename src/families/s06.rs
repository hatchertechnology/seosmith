//! S06 duplicates and soft 404s, from the saved `soft_404` and `resource` collectors.
//! Near-duplicate similarity needs retained main text, which hosted jobs do not keep.
use super::{
    common::{Ctx, SectionBuilder, capture_omissions, facts, slug},
    types::{FamilyKey, FamilySection, Outcome},
};
use crate::s06_detector;
use serde_json::json;
use std::collections::BTreeMap;

pub const SEMANTICS: &str = s06_detector::SEMANTICS;
const MAX_GROUPS: usize = 64;
const MAX_MEMBERS: usize = 16;

pub fn build(ctx: &Ctx<'_>) -> FamilySection {
    let eligible = ctx.entities.iter().filter(|e| e.html).count() as u64;
    let mut b = SectionBuilder::new(FamilyKey::S06, SEMANTICS, ctx, eligible);
    let omitted = capture_omissions(ctx, &mut b, &["soft_404", "resource"], false);
    let mut observed = 0u64;
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (n, entity) in ctx.entities.iter().enumerate() {
        let Some(capture) = ctx.captures.get(&entity.url) else {
            continue;
        };
        if omitted.contains(&n) {
            continue;
        }
        // Empty 404s and redirects all share one digest: only 2xx bodies can duplicate.
        if let Some(hash) = capture.body_hash.as_ref().filter(|_| entity.has_ok_body) {
            groups.entry(hash).or_default().push(n);
        }
        if !entity.html {
            continue;
        }
        match &capture.soft_404 {
            Some((decision, reason)) => match decision.as_str() {
                "candidate" => {
                    observed += 1;
                    b.push(
                        Some(entity),
                        "soft_404_candidate",
                        Outcome::Issue,
                        None,
                        facts(&[("detector", json!(SEMANTICS))]),
                    );
                }
                "no_candidate" => {
                    observed += 1;
                    b.push(
                        Some(entity),
                        "soft_404_clear",
                        Outcome::Pass,
                        None,
                        facts(&[]),
                    );
                }
                _ => b.push(
                    Some(entity),
                    "soft_404_undecided",
                    Outcome::Unknown,
                    Some(&slug(reason.as_deref().unwrap_or("detector_unknown"))),
                    facts(&[]),
                ),
            },
            None => b.push(
                Some(entity),
                "soft_404_undecided",
                Outcome::Unknown,
                Some("response_unavailable"),
                facts(&[]),
            ),
        }
    }
    let mut emitted = 0usize;
    let mut duplicate_groups = 0u64;
    for members in groups.values().filter(|m| m.len() > 1) {
        duplicate_groups += 1;
        if emitted == MAX_GROUPS {
            b.partial("family_item_limit");
            continue;
        }
        emitted += 1;
        let first = &ctx.entities[members[0]];
        let locators: Vec<&str> = members
            .iter()
            .take(MAX_MEMBERS)
            .map(|n| ctx.entities[*n].locator.as_str())
            .collect();
        b.push(
            Some(first),
            "exact_duplicate_group",
            Outcome::Issue,
            None,
            facts(&[
                ("members", json!(locators)),
                ("member_count", json!(members.len())),
            ]),
        );
    }
    // Every HTML page has an unknown near-duplicate result: main text is not retained.
    b.unknown_without_item("main_text_not_retained", eligible);
    b.attempted(
        ctx.entities
            .iter()
            .filter(|e| e.html && ctx.captures.contains_key(&e.url))
            .count() as u64,
    );
    b.observed(observed);
    b.summary(json!({
        "near_duplicate": {"state": "unknown", "reason": "main_text_not_retained"},
        "exact_duplicate_groups": duplicate_groups,
    }));
    b.finish()
}
