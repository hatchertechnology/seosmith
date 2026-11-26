//! S08 resource delivery and affected pages, from `resource_impact_cli`.
use super::{
    common::{Ctx, SectionBuilder, capture_omissions, facts, slug},
    types::{FamilyKey, FamilySection, Outcome},
};
use crate::resource_impact;
use serde_json::{Value, json};

pub const SEMANTICS: &str = resource_impact::SEMANTICS;
/// Decoded bytes at or above this are reported as heavy.
const HEAVY_BYTES: u64 = 1024 * 1024;
const COMPRESSIBLE_MIN_BYTES: u64 = 1024;

fn compressible(mime: Option<&str>) -> bool {
    mime.is_some_and(|m| {
        m.starts_with("text/")
            || m.contains("javascript")
            || m.contains("json")
            || m.contains("xml")
            || m.contains("svg")
    })
}

/// `no-store` or a `max-age` of zero, read as directives (`max-age=05` is 5, not 0).
fn no_cache(value: &str) -> bool {
    value.split(',').any(|d| {
        let d = d.trim().to_ascii_lowercase();
        d == "no-store"
            || d.strip_prefix("max-age=")
                .is_some_and(|n| n.trim_matches('"').parse::<u64>() == Ok(0))
    })
}

pub fn build(ctx: &Ctx<'_>, impact: &Value) -> FamilySection {
    let eligible = ctx.entities.iter().filter(|e| e.is_resource).count() as u64;
    let mut b = SectionBuilder::new(FamilyKey::S08, SEMANTICS, ctx, eligible);
    let omitted = capture_omissions(ctx, &mut b, &["resource", "resource_references"], false);
    let mut observed = 0u64;
    for r in impact["resources"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
    {
        let Some(entity) = r["url"]["url"].as_str().and_then(|u| ctx.entity(u)) else {
            continue;
        };
        if omitted.contains(&ctx.by_url[&entity.url]) {
            continue;
        }
        let bytes = r["bytes"]["decoded_body"]["value"].as_u64();
        let mime = r["mime"]["declared"]["value"].as_str();
        let affected = r["affected_pages"]["pages"].as_array().map_or(0, Vec::len)
            + usize::try_from(r["affected_pages"]["omitted_pages"].as_u64().unwrap_or(0))
                .unwrap_or(0);
        let f = facts(&[
            ("decoded_bytes", json!(bytes)),
            ("mime", json!(mime)),
            ("status", r["response"]["status"].clone()),
            ("affected_pages", json!(affected)),
        ]);
        let mut issues = 0;
        let mut unknowns = 0;
        if r["response"]["http_success"] == false {
            issues += 1;
            b.push(
                Some(entity),
                "failed_resource",
                Outcome::Issue,
                None,
                f.clone(),
            );
        }
        if bytes.is_some_and(|n| n >= HEAVY_BYTES) {
            issues += 1;
            b.push(
                Some(entity),
                "heavy_resource",
                Outcome::Issue,
                None,
                f.clone(),
            );
        }
        if r["duplicate_group_sha256"].is_string() {
            issues += 1;
            b.push(
                Some(entity),
                "duplicate_resource",
                Outcome::Issue,
                None,
                f.clone(),
            );
        }
        match r["compression"]["content_encoding"]["value"].as_str() {
            Some("identity") if compressible(mime) => {
                issues += 1;
                b.push(
                    Some(entity),
                    "uncompressed_resource",
                    Outcome::Issue,
                    None,
                    f.clone(),
                );
            }
            None if compressible(mime) && bytes.is_some_and(|n| n >= COMPRESSIBLE_MIN_BYTES) => {
                // The transport decodes bodies and may drop the header: absence proves nothing.
                unknowns += 1;
                b.push(
                    Some(entity),
                    "compression_unverified",
                    Outcome::Unknown,
                    Some("content_encoding_absent_not_proof"),
                    f.clone(),
                );
            }
            _ => {}
        }
        match r["cache"]["cache_control"]["value"].as_str() {
            Some(v) if no_cache(v) => {
                issues += 1;
                b.push(
                    Some(entity),
                    "uncached_resource",
                    Outcome::Issue,
                    None,
                    f.clone(),
                );
            }
            None => {
                unknowns += 1;
                b.push(
                    Some(entity),
                    "cache_unverified",
                    Outcome::Unknown,
                    Some("cache_control_absent_not_proof"),
                    f.clone(),
                );
            }
            _ => {}
        }
        if r["measurement"]["state"] == "observed" && bytes.is_some() {
            observed += 1;
            // A resource with an unknown dimension is observed, not a pass.
            if issues == 0 && unknowns == 0 {
                b.push(Some(entity), "resource_delivery", Outcome::Pass, None, f);
            }
        } else {
            let reason = r["bytes"]["decoded_body"]["reason"]
                .as_str()
                .unwrap_or("source_unavailable");
            b.push(
                Some(entity),
                "unmeasured_resource",
                Outcome::Unknown,
                Some(&slug(reason)),
                f,
            );
        }
    }
    for row in impact["omitted"].as_array().map_or(&[][..], Vec::as_slice) {
        let reason = row["reason"].as_str().unwrap_or("unspecified");
        if reason == "run_byte_limit" || reason == "collector_byte_limit" {
            continue;
        }
        if let Some(entity) = row["url"].as_str().and_then(|u| ctx.entity(u)) {
            let code = if reason.contains("referrer") {
                "referrer_limit".to_owned()
            } else {
                slug(reason)
            };
            b.push(
                Some(entity),
                "resource_unavailable",
                Outcome::Unknown,
                Some(&code),
                facts(&[]),
            );
        }
    }
    b.attempted(
        ctx.entities
            .iter()
            .filter(|e| e.is_resource && ctx.captures.contains_key(&e.url))
            .count() as u64,
    );
    b.observed(observed);
    if impact["row_limit_reached"] == true {
        b.partial("analyzer_row_limit");
    }
    b.summary(json!({"coverage": impact["coverage"], "bytes": {
        "decoded_body_bytes": impact["bytes"]["decoded_body_bytes"],
        "counted_resources": impact["bytes"]["counted_resources"]},
        "duplicate_groups": impact["duplicates"].as_array().map_or(0, Vec::len)}));
    b.finish()
}
