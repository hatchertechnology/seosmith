//! S05 link and navigation graph, from `saved_graph`.
use super::{
    common::{Ctx, SectionBuilder, capture_omissions, facts, slug},
    types::{FamilyKey, FamilySection, Outcome},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const SEMANTICS: &str = "seosmith-saved-graph-2";
/// A page this many clicks from the seed is reported as deep.
const DEEP: u64 = 4;

pub fn build(ctx: &Ctx<'_>, graph: &Value) -> FamilySection {
    let eligible = ctx.entities.iter().filter(|e| e.html).count() as u64;
    let mut b = SectionBuilder::new(FamilyKey::S05, SEMANTICS, ctx, eligible);
    let omitted = capture_omissions(ctx, &mut b, &["graph"], true);
    // A page whose outgoing edges are incomplete makes every distance found from the
    // other edges only an upper bound, so "deep" cannot be claimed. Edges are
    // incomplete when a capture bound dropped a graph, when the analyzer dropped a page
    // (`link_limit`, a bad row), when a page's links were truncated, and when rows
    // beyond the analyzer's row limit were never read.
    let edge_gap = edge_gap(ctx, graph, &omitted);
    let nodes = graph["nodes"].as_array().map_or(&[][..], Vec::as_slice);
    let unknown_reason = graph["coverage"]["reasons"][0]
        .as_str()
        .map_or("discovery_incomplete".to_owned(), slug);
    let mut histogram: BTreeMap<String, u64> = BTreeMap::new();
    let (mut orphans, mut sitemap_only, mut observed) = (0u64, 0u64, 0u64);
    for node in nodes {
        let Some(entity) = node["url"].as_str().and_then(|u| ctx.entity(u)) else {
            continue;
        };
        if omitted.contains(&ctx.by_url[&entity.url]) {
            continue;
        }
        let distance = node["navigation_distance"].as_u64();
        let f = facts(&[
            ("navigation_distance", json!(distance)),
            ("unique_inlinks", node["unique_inlinks"].clone()),
            ("sitemap_listed", node["sitemap_listed"].clone()),
            ("canonical_target", node["canonical_target"].clone()),
            ("page_follow", node["page_follow"].clone()),
        ]);
        if let Some(d) = distance {
            *histogram.entry(d.to_string()).or_default() += 1;
        }
        match node["reachability"].as_str() {
            Some("not_observed_reachable") => {
                orphans += 1;
                observed += 1;
                if node["sitemap_listed"] == "listed" {
                    sitemap_only += 1;
                }
                b.push(Some(entity), "orphan_page", Outcome::Issue, None, f);
            }
            Some("reachable") if distance.is_some_and(|d| d >= DEEP) && edge_gap.is_some() => {
                b.push(
                    Some(entity),
                    "deep_page",
                    Outcome::Unknown,
                    edge_gap.as_deref(),
                    f,
                );
            }
            Some("reachable") if distance.is_some_and(|d| d >= DEEP) => {
                observed += 1;
                b.push(Some(entity), "deep_page", Outcome::Issue, None, f);
            }
            Some("reachable") => {
                observed += 1;
                b.push(Some(entity), "reachable_page", Outcome::Pass, None, f);
            }
            _ => b.push(
                Some(entity),
                "reachability",
                Outcome::Unknown,
                Some(&unknown_reason),
                f,
            ),
        }
    }
    for fragment in graph["fragments"].as_array().map_or(&[][..], Vec::as_slice) {
        let Some(entity) = fragment["source"].as_str().and_then(|u| ctx.entity(u)) else {
            continue;
        };
        let f = facts(&[
            ("target", fragment["target"].clone()),
            ("fragment", fragment["fragment"].clone()),
        ]);
        match fragment["state"].as_str() {
            Some("absent") => b.push(Some(entity), "broken_fragment", Outcome::Issue, None, f),
            Some("present") => b.push(Some(entity), "fragment_present", Outcome::Pass, None, f),
            other => b.push(
                Some(entity),
                "fragment_unverified",
                Outcome::Unknown,
                other.or(Some("fragment_state_unknown")),
                f,
            ),
        }
    }
    for variant in graph["canonical_variants"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
    {
        let url = variant["source"]
            .as_str()
            .or_else(|| variant["url"].as_str());
        let Some(entity) = url.and_then(|u| ctx.entity(u)) else {
            continue;
        };
        let mut f = facts(&[]);
        if let Some(o) = variant.as_object() {
            f.extend(o.clone());
        }
        b.push(
            Some(entity),
            "canonical_variant_link",
            Outcome::Observed,
            None,
            f,
        );
    }
    let mut link_limit = false;
    for row in graph["omitted"].as_array().map_or(&[][..], Vec::as_slice) {
        let reason = row["reason"].as_str().unwrap_or("unspecified");
        let collector = row["collector_reason"].as_str();
        if matches!(collector, Some("collector_byte_limit" | "run_byte_limit"))
            || reason == "run_byte_limit"
        {
            continue; // already reported from the capture rows above
        }
        let Some(entity) = row["url"].as_str().and_then(|u| ctx.entity(u)) else {
            continue;
        };
        if !entity.html {
            continue;
        }
        let code = omitted_code(reason, collector);
        link_limit |= code == "link_limit";
        b.push(
            Some(entity),
            "graph_unavailable",
            Outcome::Unknown,
            Some(&code),
            facts(&[("analyzer_reason", json!(reason))]),
        );
    }
    b.attempted(
        ctx.entities
            .iter()
            .filter(|e| e.html && ctx.captures.contains_key(&e.url))
            .count() as u64,
    );
    b.observed(observed);
    if graph["row_limit_reached"] == true {
        b.partial("analyzer_row_limit");
    }
    if link_limit {
        b.partial("link_limit");
    }
    b.summary(json!({
        "nodes": nodes.len(), "depth_histogram": histogram, "orphans": orphans,
        "sitemap_only": sitemap_only, "analyzer_coverage": graph["coverage"],
    }));
    b.finish()
}

/// The analyzer's code for a page it dropped from the graph.
fn omitted_code(reason: &str, collector: Option<&str>) -> String {
    if reason == "analysis_budget" {
        "link_limit".to_owned()
    } else {
        slug(collector.unwrap_or(reason))
    }
}

/// The first (by name, so the choice is deterministic) reason some HTML page's outgoing
/// edges are unknown, or `None` when every page's edges were read in full.
fn edge_gap(
    ctx: &Ctx<'_>,
    graph: &Value,
    capture_omitted: &std::collections::HashSet<usize>,
) -> Option<String> {
    let mut codes = std::collections::BTreeSet::new();
    for n in capture_omitted {
        let reason = ctx
            .captures
            .get(&ctx.entities[*n].url)
            .and_then(|c| c.omission("graph"));
        codes.insert(reason.unwrap_or("collector_byte_limit").to_owned());
    }
    for row in graph["omitted"].as_array().map_or(&[][..], Vec::as_slice) {
        let reason = row["reason"].as_str().unwrap_or("unspecified");
        if row["url"]
            .as_str()
            .and_then(|u| ctx.entity(u))
            .is_some_and(|e| e.html)
        {
            codes.insert(omitted_code(reason, row["collector_reason"].as_str()));
        }
    }
    if graph["row_limit_reached"] == true {
        codes.insert("analyzer_row_limit".to_owned());
    }
    let reasons = graph["coverage"]["reasons"].as_array();
    if reasons.is_some_and(|r| {
        r.iter().any(|c| {
            matches!(
                c.as_str(),
                Some("extraction_truncated" | "truncation_counts_invalid")
            )
        })
    }) {
        codes.insert("link_limit".to_owned());
    }
    codes.into_iter().next()
}
