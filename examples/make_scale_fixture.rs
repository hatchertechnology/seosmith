//! Synthesizes a base (schema 1) report at the geometry of the seosmith#64
//! failure: 57 in-scope pages plus up to 1,696 in-scope resources, each page
//! carrying out-of-scope links (the 14c calibration is about 8 per page).
//!
//! The builder is shared with `tests/wire_scale.rs` (which includes this file
//! with `#[path]`), so the test and the generator use exactly one recipe.
//!
//! ```text
//! cargo run --locked --example make_scale_fixture --features policy-wire-prototype -- \
//!     [resources=1696] [external_links_per_page=8] > scale.json
//! ```
#![allow(dead_code)]
use anyhow::Result;
use seosmith::model::{CrawlRequest, Link, Page, Relationship, Report};

pub const SCALE_PAGES: usize = 57;
pub const SCALE_HOST: &str = "https://scale.example";

#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub pages: usize,
    pub resources: usize,
    /// Out-of-scope links on every in-scope page; each becomes one S14 edge id.
    pub external_links_per_page: usize,
}

impl Shape {
    #[must_use]
    pub const fn site(resources: usize, external_links_per_page: usize) -> Self {
        Self {
            pages: SCALE_PAGES,
            resources,
            external_links_per_page,
        }
    }
}

const KINDS: [(&str, &str, &str); 4] = [
    ("stylesheet", "css", "text/css"),
    ("script", "js", "text/javascript"),
    ("image", "png", "image/png"),
    ("font", "woff2", "font/woff2"),
];

/// Base report for `shape`, ready for `generate_wire_v1` (schema 1, no extensions).
///
/// # Errors
/// Fails only if the fixed request literal stops parsing.
pub fn scale_report(shape: Shape) -> Result<Report> {
    let request: CrawlRequest = serde_json::from_value(serde_json::json!({
        "request_id": "00000000-0000-4000-8000-0000000000a1",
        "website_id": "00000000-0000-4000-8000-0000000000a2",
        "binding_generation": 1,
        "url": format!("{SCALE_HOST}/"),
        "max_pages": 100,
        "max_duration_seconds": 3600,
        "delay_ms": 1000,
        "check_external_links": false,
        "max_resources": 3000
    }))?;
    let mut pages = Vec::with_capacity(shape.pages + shape.resources);
    for i in 0..shape.pages {
        let mut links = Vec::new();
        for step in 1..=3 {
            links.push(Link {
                url: format!("{SCALE_HOST}/p{}", (i + step) % shape.pages),
                text: format!("Internal {step}"),
                ..Link::default()
            });
        }
        for j in 0..shape.external_links_per_page {
            links.push(Link {
                url: format!("https://external.example/{i}/{j}"),
                text: format!("External {j}"),
                ..Link::default()
            });
        }
        let relationships = (i..shape.resources)
            .step_by(shape.pages.max(1))
            .map(|k| {
                let (kind, ext, _) = KINDS[k % KINDS.len()];
                Relationship {
                    kind: kind.into(),
                    url: format!("{SCALE_HOST}/assets/r{k}.{ext}"),
                    descriptor: String::new(),
                    resource: true,
                    state: "fetched".into(),
                }
            })
            .collect();
        pages.push(Page {
            url: if i == 0 {
                format!("{SCALE_HOST}/")
            } else {
                format!("{SCALE_HOST}/p{i}")
            },
            depth: usize::from(i > 0),
            status: Some(200),
            content_type: "text/html".into(),
            bytes: 19_400,
            title: format!("Scale fixture page number {i} of the site"),
            description: format!(
                "Synthetic description for page {i}, long enough to satisfy the default minimum."
            ),
            language: "en".into(),
            headings: vec![(1, format!("Heading {i}"))],
            links,
            relationships,
            words: 400,
            encoding: "utf-8".into(),
            ..Page::default()
        });
    }
    for k in 0..shape.resources {
        let (_, ext, content_type) = KINDS[k % KINDS.len()];
        pages.push(Page {
            url: format!("{SCALE_HOST}/assets/r{k}.{ext}"),
            depth: 1,
            status: Some(200),
            content_type: content_type.into(),
            bytes: 783,
            is_resource: true,
            ..Page::default()
        });
    }
    Ok(Report {
        schema_version: 1,
        engine_version: "0.2.0".into(),
        ruleset_version: "2026-11-01.1".into(),
        request,
        started_at: "2026-11-26T12:00:00Z".into(),
        finished_at: "2026-11-26T12:30:00Z".into(),
        outcome: "completed".into(),
        stop_reason: "frontier_exhausted".into(),
        coverage_complete: true,
        pages,
        findings: vec![],
        evidence_contract: None,
        policy_guidance: None,
    })
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let resources = args.next().map_or(Ok(1696), |v| v.parse())?;
    let external = args.next().map_or(Ok(8), |v| v.parse())?;
    let report = scale_report(Shape::site(resources, external))?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
