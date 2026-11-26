//! S03 canonical, hreflang and sitemap, from `saved_index_integrity`.
use super::{
    common::{Ctx, SectionBuilder, capture_omissions},
    s02::push_findings,
    types::{FamilyKey, FamilySection},
};
use serde_json::{Value, json};

pub const SEMANTICS: &str = "s03-integrity-1";
const MAX_SOURCES: usize = 20;

pub fn is_s03_rule(rule: &str) -> bool {
    rule.starts_with("canonical_") || rule.starts_with("hreflang_") || rule.starts_with("sitemap_")
}

pub fn build(ctx: &Ctx<'_>, index: &Value) -> FamilySection {
    let eligible = ctx.entities.iter().filter(|e| e.html).count() as u64;
    let mut b = SectionBuilder::new(FamilyKey::S03, SEMANTICS, ctx, eligible);
    let findings = index["findings"].as_array().map_or(&[][..], Vec::as_slice);
    let (_, decided) = push_findings(ctx, &mut b, findings, is_s03_rule);
    let omitted = capture_omissions(ctx, &mut b, &["html_occurrences"], true);
    b.attempted(
        ctx.entities
            .iter()
            .filter(|e| e.html && ctx.captures.contains_key(&e.url))
            .count() as u64,
    );
    b.observed(decided.iter().filter(|n| !omitted.contains(n)).count() as u64);
    if index["coverage"]["row_limit_reached"] == true {
        b.partial("analyzer_row_limit");
    }
    let sitemap = &index["graphs"]["sitemap"];
    if sitemap["omitted_sources"].as_u64().unwrap_or(0) > 0
        || sitemap["remaining_sources_unknown"] == true
    {
        b.partial("sitemap_lookup_limit");
    }
    let sources: Vec<Value> = sitemap["sources"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .take(MAX_SOURCES)
        .map(|s| {
            json!({"url": s["url"], "state": s["state"], "status": s["status"],
            "entries": s["document"]["entries"]})
        })
        .collect();
    b.summary(json!({
        "sitemap": {"state": sitemap["state"], "discovery_state": sitemap["discovery_state"],
            "roots": sitemap["roots"].as_array().map_or(0, Vec::len),
            "omitted_sources": sitemap["omitted_sources"], "sources": sources},
        "canonical_graphs": index["graphs"]["canonical"].as_array().map_or(0, Vec::len),
        "alternate_clusters": index["graphs"]["alternates"].as_array().map_or(0, Vec::len),
    }));
    b.finish()
}
