//! S04 structured data, from `structured_validation::validate_crawl` on the crawled pages.
use super::{
    common::{Ctx, SectionBuilder, capture_omissions, facts, slug},
    types::{FamilyKey, FamilySection, Outcome},
};
use crate::{model::Report, structured_validation};
use serde_json::json;
use std::collections::BTreeMap;

pub const SEMANTICS: &str = structured_validation::PROFILE_VERSION;
/// Observations kept per page: the rest are counted in `facts.more`.
const PER_PAGE: usize = 8;

pub fn build(ctx: &Ctx<'_>, report: &Report, started: std::time::Instant) -> FamilySection {
    let eligible = ctx.entities.iter().filter(|e| e.html).count() as u64;
    let mut b = SectionBuilder::new(FamilyKey::S04, SEMANTICS, ctx, eligible);
    let omitted = capture_omissions(ctx, &mut b, &["html_occurrences"], true);
    let mut with_data = 0u64;
    let mut observed = 0u64;
    let mut by_code: BTreeMap<String, u64> = BTreeMap::new();
    for (n, (entity, page)) in ctx.entities.iter().zip(&report.pages).enumerate() {
        if !entity.html {
            continue;
        }
        if ctx.family_expired(started) {
            b.deadline_hit(1);
            continue;
        }
        let observations = structured_validation::validate_crawl(page, &report.pages);
        if observations.iter().any(|o| o.code == "page_unavailable") {
            continue;
        }
        observed += 1;
        let (coverage, findings): (Vec<_>, Vec<_>) =
            observations.iter().partition(|o| o.category == "coverage");
        for o in &coverage {
            *by_code.entry(slug(o.code)).or_default() += 1;
        }
        let blocks = page.structured_data.len();
        if blocks > 0 {
            with_data += 1;
        }
        for o in findings.iter().take(PER_PAGE) {
            *by_code.entry(slug(o.code)).or_default() += 1;
            let mut f = facts(&[
                ("category", json!(o.category)),
                ("code", json!(o.code)),
                ("path", json!(o.path)),
                ("explanation", json!(o.explanation)),
            ]);
            if let Some(v) = &o.value {
                f.insert("value".into(), v.clone());
            }
            b.push(Some(entity), &slug(o.code), Outcome::Issue, None, f);
        }
        if findings.len() > PER_PAGE {
            b.push(
                Some(entity),
                "more_observations",
                Outcome::Observed,
                None,
                facts(&[("more", json!(findings.len() - PER_PAGE))]),
            );
        }
        if let Some(limit) = coverage.first() {
            b.push(
                Some(entity),
                "profile_coverage",
                Outcome::Unknown,
                Some(if limit.code.contains("unsupported") {
                    "profile_unsupported"
                } else {
                    limit.code
                }),
                facts(&[
                    ("code", json!(limit.code)),
                    ("explanation", json!(limit.explanation)),
                ]),
            );
        } else if findings.is_empty() && blocks > 0 && !omitted.contains(&n) {
            b.push(
                Some(entity),
                "structured_data_valid",
                Outcome::Pass,
                None,
                facts(&[("blocks", json!(blocks))]),
            );
        }
    }
    b.attempted(ctx.entities.iter().filter(|e| e.html).count() as u64);
    b.observed(observed.saturating_sub(omitted.len() as u64));
    b.summary(
        json!({"profile": SEMANTICS, "pages_with_structured_data": with_data,
        "observation_codes": by_code}),
    );
    b.finish()
}
