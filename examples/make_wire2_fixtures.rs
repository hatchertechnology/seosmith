//! Generates `fixtures/wire2/**`: s14-wire-2 positives and negatives (design
//! `wave1-wp8a.md` section 7, "Fixtures and parity"; contract
//! `docs/contracts/s14-wire-2.md`).
//!
//! Every positive is the exact byte string `wire2::generate_wire_v2` returns
//! (the producer's serialiser, budgeter and invariant guard), over a base report
//! and a `FamilySet` assembled with the real section builder
//! (`families::common::SectionBuilder`). Every negative is one positive (or one
//! wire-1 twin) with one defect applied to the parsed value and re-serialised
//! with the same compact, key-sorted `serde_json` writer. Nothing is edited by
//! hand: re-running this example reproduces every file byte for byte, and
//! `tests/wire2_fixtures.rs` proves it.
//!
//! The expected `admit` outcome of every file is the design's section 5 stage
//! table. `scripts/verify-s14-schema4.py --wire2 --admit <bin>` runs the HTP
//! receiver on each file and requires the same stage and error name.
//!
//! ```text
//! cargo +1.98.1 run --locked --release --features policy-wire-prototype \
//!     --example make_wire2_fixtures [output-dir=fixtures/wire2]
//! ```
#![allow(dead_code)]
use anyhow::{Context, Result, bail, ensure};
use seosmith::{
    delivery::HTP_MAX_REPORT_BYTES,
    families::{
        common::{Ctx, SectionBuilder, facts},
        s02, s03, s04, s05, s06, s08,
        types::{
            CaptureMode, FAMILY_LIMITS, FamilyItem, FamilyKey, FamilySection, FamilySet, Outcome,
            SystemClock,
        },
    },
    finding_coverage_prototype::{fixture_input, generate_wire_capped, generate_wire_v1},
    model::{Page, Report},
    policy::AdvisoryPolicy,
    policy_wire_prototype::WireProfile,
    wire2::{generate_wire_v2, generate_wire_v2_with_cap},
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[path = "make_scale_fixture.rs"]
mod scale;

pub const DEFAULT_DIR: &str = "fixtures/wire2";
pub const V1_REVISION: &str = "s14-wire-1/full-population-1/evidence-v1";
pub const V2_REVISION: &str = "s14-wire-2/full-population-1/evidence-v1/families-v1";
const RESERVE: usize = 65_536;
static CLOCK: SystemClock = SystemClock;

/// What `admit` must answer for a fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Admit(&'static str),
    Reject(&'static str, &'static str),
}

pub struct Case {
    pub path: String,
    pub base: &'static str,
    pub wire: &'static str,
    pub bytes: Vec<u8>,
    pub expect: Expect,
    pub note: &'static str,
}

// ------------------------------------------------------------------ bases

fn page(url: &str, title: &str) -> Page {
    Page {
        url: url.into(),
        status: Some(200),
        content_type: "text/html".into(),
        encoding: "utf-8".into(),
        language: "en".into(),
        language_source: "html".into(),
        title: title.into(),
        description: "A page description that is long enough to be a description, but not too long for the rules.".into(),
        headings: vec![(1, "A heading".into())],
        bytes: 1024,
        words: 120,
        ..Page::default()
    }
}

fn with_pages(case: &str, pages: Vec<Page>) -> Result<Report> {
    let mut report = fixture_input(case)?;
    report.pages = pages;
    Ok(report)
}

/// `n` HTML pages, every fifth with a title (the rest have none, so S14 has
/// findings to rebuild): enough pages to pass the wire-1 limits of 128
/// observations and 512 evaluations.
fn site(n: usize) -> Result<Report> {
    let pages = (0..n)
        .map(|i| {
            page(
                &format!("https://example.com/p{i}"),
                if i % 5 == 0 {
                    "A fine title for the page"
                } else {
                    ""
                },
            )
        })
        .collect();
    with_pages("complete-clean-h1", pages)
}

/// 150 pages, all without a title: more than 128 observations and more than 512
/// evaluations, still under the wire-2 limits of 1,024.
fn limits_site() -> Result<Report> {
    let pages = (0..150)
        .map(|i| Page {
            url: format!("https://example.com/p{i}"),
            status: Some(200),
            content_type: "text/html".into(),
            encoding: "utf-8".into(),
            ..Page::default()
        })
        .collect();
    with_pages("complete-clean-h1", pages)
}

/// 200 pages with mixed source states: S14 emits more than 512 records.
fn mixed_caps() -> Result<Report> {
    let mut pages = Vec::new();
    for i in 0..200 {
        let mut p = page(
            &format!("https://example.com/p{i}"),
            if i % 5 == 0 {
                "A fine title for the page"
            } else {
                ""
            },
        );
        match i % 17 {
            3 => p.status = Some(404),
            7 => p.content_type = "application/json".into(),
            11 => {
                p.status = None;
                p.blocked_by_robots = true;
            }
            _ => {}
        }
        pages.push(p);
    }
    with_pages("complete-clean-h1", pages)
}

fn external_links_checked() -> Result<Report> {
    let full = format!(
        "{}/fixtures/policies/v3-local-complete.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut report: Report = serde_json::from_str(&std::fs::read_to_string(&full)?)?;
    report.schema_version = 1;
    report.policy_guidance = None;
    report.evidence_contract = None;
    report.findings.clear();
    Ok(report)
}

/// One page with 23 links and 23 relationships: the last locators are
/// `/pages/0/links/22` and `/pages/0/relationships/22`.
fn rich_site() -> Result<Report> {
    scale::scale_report(scale::Shape {
        pages: 1,
        resources: 23,
        external_links_per_page: 20,
    })
}

fn tiny() -> Result<Report> {
    scale::scale_report(scale::Shape {
        pages: 6,
        resources: 8,
        external_links_per_page: 2,
    })
}

// --------------------------------------------------------------- families

fn entity_indexes(report: &Report) -> (Vec<usize>, Vec<usize>) {
    let html = (0..report.pages.len())
        .filter(|n| report.pages[*n].html() && !report.pages[*n].is_resource)
        .collect();
    let res = (0..report.pages.len())
        .filter(|n| report.pages[*n].is_resource)
        .collect();
    (html, res)
}

fn semantics(key: FamilyKey) -> &'static str {
    match key {
        FamilyKey::S02 => s02::SEMANTICS,
        FamilyKey::S03 => s03::SEMANTICS,
        FamilyKey::S04 => s04::SEMANTICS,
        FamilyKey::S05 => s05::SEMANTICS,
        FamilyKey::S06 => s06::SEMANTICS,
        FamilyKey::S08 => s08::SEMANTICS,
    }
}

const BOTS: [&str; 5] = ["googlebot", "bingbot", "duckduckbot", "gptbot", "claudebot"];

/// Which of the section scenarios to assemble.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sc {
    /// Five sections complete; S06 partial, because near-duplicate similarity is
    /// always `unknown` (`main_text_not_retained`), as the real builder emits.
    Complete,
    /// Partial sections with reasons, one unavailable.
    Partial,
    /// Capture omissions (`collector_byte_limit`, `run_byte_limit`).
    Omissions,
    /// S06 has more items than its `max_items` (128).
    ItemLimit,
    /// Items large enough that the byte allowance cuts them.
    Heavy,
    /// One item with a null locator and `facts.url`.
    Unbound,
    /// Items at the last link and relationship of page 0.
    Locators,
}

fn pad_facts(pad: usize, i: usize) -> Map<String, Value> {
    facts(&[("pad", json!("p".repeat(pad))), ("i", json!(i))])
}

fn families(report: &Report, sc: Sc) -> FamilySet {
    let ctx = Ctx::new(report, HashMap::new(), &CLOCK, CaptureMode::Wire2);
    let (html, res) = entity_indexes(report);
    let ent = |n: usize| &ctx.entities[n];
    let eligible = html.len() as u64;
    let mut set: Vec<(FamilyKey, FamilySection)> = Vec::new();

    // S02 indexing and crawler permissions.
    let mut b = SectionBuilder::new(FamilyKey::S02, s02::SEMANTICS, &ctx, eligible);
    for (k, n) in html.iter().enumerate() {
        let restricted = k + 1 == html.len() && html.len() > 1;
        b.push(
            Some(ent(*n)),
            if restricted {
                "index_restricted"
            } else {
                "index_allowed"
            },
            if restricted {
                Outcome::Issue
            } else {
                Outcome::Pass
            },
            None,
            facts(&[
                (
                    "rule",
                    json!(if restricted {
                        "index_restricted"
                    } else {
                        "index_allowed"
                    }),
                ),
                ("detail", json!("")),
                ("bots", json!(BOTS)),
            ]),
        );
    }
    b.attempted(eligible);
    b.observed(eligible);
    b.summary(json!({"bots": BOTS, "analyzer_state": "complete",
        "rows_seen": report.pages.len(), "robots": "present"}));
    let mut s02_section = b.finish();
    if sc == Sc::Partial {
        let mut b = SectionBuilder::new(FamilyKey::S02, s02::SEMANTICS, &ctx, eligible);
        b.push(
            Some(ent(html[0])),
            "crawl_unknown",
            Outcome::Unknown,
            Some("robots_unavailable"),
            facts(&[
                ("rule", json!("crawl_unknown")),
                ("detail", json!("robots.txt was not retrieved")),
                ("bots", json!(["googlebot"])),
            ]),
        );
        b.push(
            Some(ent(html[1])),
            "index_allowed",
            Outcome::Pass,
            None,
            facts(&[
                ("rule", json!("index_allowed")),
                ("detail", json!("")),
                ("bots", json!(BOTS)),
            ]),
        );
        b.partial("analyzer_row_limit");
        b.attempted(eligible);
        b.observed(1);
        b.summary(json!({"bots": BOTS, "analyzer_state": "partial",
            "rows_seen": 1000, "robots": "unavailable"}));
        s02_section = b.finish();
    }
    set.push((FamilyKey::S02, s02_section));

    // S03 canonical, hreflang and sitemap.
    let mut b = SectionBuilder::new(FamilyKey::S03, s03::SEMANTICS, &ctx, eligible);
    for (k, n) in html.iter().enumerate() {
        if k == 1 {
            b.push(
                Some(ent(*n)),
                "canonical_conflicting",
                Outcome::Issue,
                None,
                facts(&[
                    ("rule", json!("canonical_conflicting")),
                    ("detail", json!("two canonicals")),
                ]),
            );
        } else {
            b.push(
                Some(ent(*n)),
                "canonical_absent",
                Outcome::Pass,
                None,
                facts(&[("rule", json!("canonical_absent")), ("detail", json!(""))]),
            );
        }
    }
    b.attempted(eligible);
    b.observed(eligible);
    b.summary(
        json!({"sitemap": {"state": "complete", "discovery_state": "complete",
        "roots": 1, "omitted_sources": 0, "sources": []},
        "sitemap_flag_mismatch_pages": 0, "canonical_graphs": 0, "alternate_clusters": 0}),
    );
    if sc == Sc::Unbound {
        b.push(
            None,
            "sitemap_entry_not_crawled",
            Outcome::Issue,
            None,
            facts(&[("url", json!("https://example.com/in-sitemap-only"))]),
        );
    }
    let mut s03_section = b.finish();
    if sc == Sc::Partial {
        let mut b = SectionBuilder::new(FamilyKey::S03, s03::SEMANTICS, &ctx, eligible);
        b.push(
            Some(ent(html[0])),
            "sitemap_origin_unavailable",
            Outcome::Unknown,
            Some("target_not_found_in_retained_documents"),
            facts(&[
                ("rule", json!("sitemap_origin_unavailable")),
                ("detail", json!("target_not_found_in_retained_documents")),
            ]),
        );
        b.partial("sitemap_evidence_mismatch");
        b.attempted(eligible);
        b.observed(0);
        b.summary(
            json!({"sitemap": {"state": "partial", "discovery_state": "complete",
            "roots": 1, "omitted_sources": 0, "sources": []},
            "sitemap_flag_mismatch_pages": 1, "canonical_graphs": 0, "alternate_clusters": 0}),
        );
        s03_section = b.finish();
    }
    set.push((FamilyKey::S03, s03_section));

    // S04 structured data.
    let mut b = SectionBuilder::new(FamilyKey::S04, s04::SEMANTICS, &ctx, eligible);
    for (k, n) in html.iter().enumerate() {
        if k % 3 == 0 {
            b.push(
                Some(ent(*n)),
                "structured_data_valid",
                Outcome::Pass,
                None,
                facts(&[("blocks", json!(1))]),
            );
        } else if k % 3 == 1 {
            b.push(
                Some(ent(*n)),
                "missing_required_property",
                Outcome::Issue,
                None,
                facts(&[
                    ("category", json!("eligibility")),
                    ("code", json!("missing_required_property")),
                    ("path", json!("/0/name")),
                    ("explanation", json!("Product requires name")),
                ]),
            );
        }
    }
    b.attempted(eligible);
    b.observed(eligible);
    b.summary(
        json!({"profile": s04::SEMANTICS, "pages_with_structured_data": html.len(), "codes": {}}),
    );
    let s04_section = if sc == Sc::Partial {
        FamilySection::unavailable(FamilyKey::S04, s04::SEMANTICS, "analysis_deadline")
    } else {
        b.finish()
    };
    set.push((FamilyKey::S04, s04_section));

    // S05 link and navigation graph.
    let mut b = SectionBuilder::new(FamilyKey::S05, s05::SEMANTICS, &ctx, eligible);
    if sc == Sc::Omissions {
        for (k, n) in html.iter().take(3).enumerate() {
            b.push(
                Some(ent(*n)),
                "capture_omission",
                Outcome::Unknown,
                Some("collector_byte_limit"),
                facts(&[(
                    "collector",
                    json!(["graph", "html_occurrences", "resource"][k % 3]),
                )]),
            );
        }
        b.partial("collector_byte_limit");
        b.attempted(eligible);
        b.observed(0);
    } else {
        for (k, n) in html.iter().enumerate() {
            let orphan = k + 1 == html.len() && html.len() > 1;
            b.push(
                Some(ent(*n)),
                if orphan {
                    "orphan_page"
                } else {
                    "reachable_page"
                },
                if orphan {
                    Outcome::Issue
                } else {
                    Outcome::Pass
                },
                None,
                facts(&[
                    (
                        "navigation_distance",
                        json!(if orphan { Value::Null } else { json!(k) }),
                    ),
                    ("unique_inlinks", json!(if orphan { 0 } else { 2 })),
                    ("sitemap_listed", json!(!orphan)),
                    ("page_follow", json!(true)),
                ]),
            );
        }
        if sc == Sc::Partial {
            b.push(
                Some(ent(html[0])),
                "deep_page",
                Outcome::Unknown,
                Some("link_limit"),
                facts(&[("analyzer_reason", json!("link_limit"))]),
            );
            b.partial("link_limit");
        }
        b.attempted(eligible);
        b.observed(eligible);
    }
    b.summary(
        json!({"nodes": html.len(), "edges": html.len().saturating_sub(1),
        "orphans": 1, "depth_histogram": {"0": 1}}),
    );
    if sc == Sc::Locators {
        let mut s = b.finish();
        let pg = &report.pages[0];
        let eid = seosmith::contract::entity_id(&pg.url, pg.is_resource);
        for locator in ["/pages/0/links/22", "/pages/0/relationships/22"] {
            let kind = if locator.contains("links") {
                "canonical_variant_link"
            } else {
                "broken_fragment"
            };
            add_item(
                &mut s,
                FamilyItem {
                    entity_id: Some(eid.clone()),
                    locator: Some(locator.into()),
                    kind: kind.into(),
                    outcome: Outcome::Issue,
                    reason: None,
                    facts: facts(&[("target", json!("https://scale.example/p0"))]),
                },
            );
        }
        set.push((FamilyKey::S05, s));
    } else {
        set.push((FamilyKey::S05, b.finish()));
    }

    // S06 duplicates and soft 404s.
    let mut b = SectionBuilder::new(FamilyKey::S06, s06::SEMANTICS, &ctx, eligible);
    let soft = if sc == Sc::ItemLimit {
        html.len()
    } else {
        html.len().min(20)
    };
    for n in html.iter().take(soft) {
        b.push(
            Some(ent(*n)),
            "soft_404_clear",
            Outcome::Pass,
            None,
            facts(&[]),
        );
    }
    if html.len() >= 3 {
        b.push(
            Some(ent(html[0])),
            "exact_duplicate_group",
            Outcome::Issue,
            None,
            facts(&[
                (
                    "members",
                    json!([ent(html[0]).locator, ent(html[2]).locator]),
                ),
                ("member_count", json!(2)),
            ]),
        );
    }
    if sc == Sc::ItemLimit {
        for n in &html {
            b.push(
                Some(ent(*n)),
                "soft_404_candidate",
                Outcome::Issue,
                None,
                facts(&[("detector", json!(s06::SEMANTICS))]),
            );
        }
    }
    b.unknown_without_item("main_text_not_retained", eligible);
    b.attempted(eligible);
    b.observed(soft as u64);
    b.summary(
        json!({"near_duplicate": {"state": "unknown", "reason": "main_text_not_retained"},
        "exact_duplicate_groups": 1}),
    );
    set.push((FamilyKey::S06, b.finish()));

    // S08 resource delivery.
    let mut b = SectionBuilder::new(FamilyKey::S08, s08::SEMANTICS, &ctx, res.len() as u64);
    for (k, n) in res.iter().enumerate() {
        let heavy = k % 7 == 3;
        b.push(
            Some(ent(*n)),
            if heavy {
                "heavy_resource"
            } else {
                "resource_delivery"
            },
            if heavy { Outcome::Issue } else { Outcome::Pass },
            None,
            facts(&[
                ("decoded_bytes", json!(if heavy { 900_000 } else { 783 })),
                ("mime", json!("text/css")),
                ("status", json!(200)),
                ("affected_pages", json!(1)),
            ]),
        );
    }
    b.attempted(res.len() as u64);
    b.observed(res.len() as u64);
    b.summary(json!({"resources": res.len(), "measured": res.len()}));
    let s08_section = if sc == Sc::Omissions {
        FamilySection::unavailable(FamilyKey::S08, s08::SEMANTICS, "run_byte_limit")
    } else {
        b.finish()
    };
    set.push((FamilyKey::S08, s08_section));

    let mut set = FamilySet(set.into_iter().collect());
    if sc == Sc::Omissions {
        // A second family carrying a run-limit omission as an item.
        let s = set.0.get_mut(&FamilyKey::S02).expect("s02");
        let n = html[0];
        let mut b = SectionBuilder::new(FamilyKey::S02, s02::SEMANTICS, &ctx, eligible);
        b.push(
            Some(ent(n)),
            "capture_omission",
            Outcome::Unknown,
            Some("run_byte_limit"),
            facts(&[("collector", json!("html_occurrences"))]),
        );
        b.partial("run_byte_limit");
        b.attempted(eligible);
        b.observed(0);
        b.summary(
            json!({"bots": BOTS, "analyzer_state": "partial", "rows_seen": 0, "robots": "present"}),
        );
        *s = b.finish();
    }
    if sc == Sc::Heavy {
        for key in [FamilyKey::S03, FamilyKey::S04, FamilyKey::S05] {
            let mut b = SectionBuilder::new(key, semantics(key), &ctx, eligible);
            for (i, n) in html.iter().enumerate().take(128) {
                for kind in ["alpha_item", "beta_item"] {
                    b.push(
                        Some(ent(*n)),
                        kind,
                        Outcome::Issue,
                        None,
                        pad_facts(1_500, i),
                    );
                }
            }
            set.0.insert(key, b.finish());
        }
    }
    set
}

/// Sets every family section to heavy content, so a tight report budget has
/// something to cut.
fn heavy_everywhere(report: &Report) -> FamilySet {
    let mut set = families(report, Sc::Complete);
    let ctx = Ctx::new(report, HashMap::new(), &CLOCK, CaptureMode::Wire2);
    let (html, _) = entity_indexes(report);
    for (key, limits) in FAMILY_LIMITS {
        let mut b = SectionBuilder::new(key, semantics(key), &ctx, html.len() as u64);
        let mut count = 0;
        'outer: for (i, n) in html.iter().enumerate() {
            for kind in ["alpha_item", "beta_item"] {
                if count == limits.max_items {
                    break 'outer;
                }
                b.push(
                    Some(&ctx.entities[*n]),
                    kind,
                    Outcome::Issue,
                    None,
                    pad_facts(1_500, i),
                );
                count += 1;
            }
        }
        set.0.insert(key, b.finish());
    }
    set
}

/// Adds an item to a finished section and keeps the bookkeeping and the order
/// (outcome rank, locator with null first, kind) the contract requires.
fn add_item(section: &mut FamilySection, item: FamilyItem) {
    section.items.push(item);
    section
        .items
        .sort_by(|a, b| (a.outcome, &a.locator, &a.kind).cmp(&(b.outcome, &b.locator, &b.kind)));
    section.items_total += 1;
}

// -------------------------------------------------------------- positives

fn gen_v2(report: &Report, set: FamilySet) -> Result<Vec<u8>> {
    let mut report = report.clone();
    Ok(generate_wire_v2(
        &mut report,
        &AdvisoryPolicy::default(),
        set,
    )?)
}

fn parse(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice(bytes)?)
}

fn ser(value: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

/// `total` bytes exactly, by padding one `open_graph` value per page (ASCII,
/// one byte per character), through `generate_wire_v2_with_cap` with a cap one
/// reserve above the HTP cap. The production cap keeps a 65,536 B reserve, so
/// the production producer never emits a report this close to the cap; the
/// fixture exists for the receiver's own boundary.
fn padded_to(total: usize) -> Result<Vec<u8>> {
    const PAGES: usize = 60;
    const KEYS: usize = 6;
    const MAX_PAD: usize = 15_000;
    let build = |pads: &[usize]| -> Result<Vec<u8>> {
        let pages = (0..PAGES)
            .map(|i| {
                let mut p = page(
                    &format!("https://example.com/cap/{i:04}"),
                    &format!("A well sized title for cap page {i:04}"),
                );
                for k in 0..KEYS {
                    p.open_graph
                        .insert(format!("og:pad{k}"), vec!["x".repeat(pads[i * KEYS + k])]);
                }
                p
            })
            .collect();
        let report = with_pages("complete-clean-h1", pages)?;
        let set = families(&report, Sc::Complete);
        let mut report = report;
        Ok(generate_wire_v2_with_cap(
            &mut report,
            &AdvisoryPolicy::default(),
            set,
            HTP_MAX_REPORT_BYTES + RESERVE + 786_432,
        )?)
    };
    let mut pads = vec![12_000; PAGES * KEYS];
    for _ in 0..4 {
        let bytes = build(&pads)?;
        if bytes.len() == total {
            return Ok(bytes);
        }
        let mut grow = total.saturating_sub(bytes.len());
        let mut shrink = bytes.len().saturating_sub(total);
        for pad in pads.iter_mut().rev() {
            let up = grow.min(MAX_PAD - *pad);
            *pad += up;
            grow -= up;
            let down = shrink.min(*pad);
            *pad -= down;
            shrink -= down;
            if grow == 0 && shrink == 0 {
                break;
            }
        }
        ensure!(grow + shrink == 0, "cannot pad to {total} B");
    }
    let bytes = build(&pads)?;
    ensure!(
        bytes.len() == total,
        "padding to {total} B did not converge"
    );
    Ok(bytes)
}

/// The size of the wire-2 base (everything but `evidence_families`).
fn base_len(report: &Report) -> Result<usize> {
    let mut report = report.clone();
    Ok(generate_wire_capped(
        &mut report,
        &AdvisoryPolicy::default(),
        WireProfile::WIRE2,
        HTP_MAX_REPORT_BYTES,
    )?
    .len())
}

struct Out {
    cases: Vec<Case>,
}

impl Out {
    fn pos(&mut self, name: &str, base: &'static str, bytes: Vec<u8>, note: &'static str) {
        self.cases.push(Case {
            path: format!("positive/{name}.json"),
            base,
            wire: "s14-wire-2",
            bytes,
            expect: Expect::Admit(V2_REVISION),
            note,
        });
    }
    fn neg(
        &mut self,
        name: &str,
        base: &'static str,
        wire: &'static str,
        value: &Value,
        expect: Expect,
        note: &'static str,
    ) -> Result<()> {
        self.cases.push(Case {
            path: format!("negative/{name}.json"),
            base,
            wire,
            bytes: ser(value)?,
            expect,
            note,
        });
        Ok(())
    }
}

fn section_mut<'a>(v: &'a mut Value, key: &str) -> &'a mut Value {
    &mut v["evidence_families"]["families"][key]
}

fn first_item_mut<'a>(v: &'a mut Value, key: &str) -> Result<&'a mut Value> {
    section_mut(v, key)["items"]
        .get_mut(0)
        .with_context(|| format!("{key} has no items"))
}

fn rows(v: &mut Value) -> &mut Vec<Value> {
    v["policy_guidance"]["manifest"].as_array_mut().unwrap()
}

fn digest_of(ids: &[String]) -> String {
    seosmith::policy_candidate::candidates_digest(ids)
}

/// Builds every case (positives first, then negatives) in a fixed order.
///
/// # Panics
/// Only if a base report no longer has the shape a mutation expects (a bug in this generator).
pub fn cases() -> Result<Vec<Case>> {
    let mut out = Out { cases: Vec::new() };
    let policy = AdvisoryPolicy::default();

    // ---- positives
    let tiny = tiny()?;
    let complete = gen_v2(&tiny, families(&tiny, Sc::Complete))?;
    out.pos(
        "complete-small",
        "scale(6 pages, 8 resources)",
        complete.clone(),
        "five families complete; S06 partial (near-duplicate similarity is always unknown, main_text_not_retained)",
    );

    let partial_base = fixture_input("partial-missing-h1")?;
    let partial_base = {
        let mut r = partial_base;
        // fixture_input keeps one page; give the families two HTML pages.
        let second = page(
            "https://example.com/second",
            "Second page title for the fixture",
        );
        r.pages.push(second);
        r
    };
    let partial = gen_v2(&partial_base, families(&partial_base, Sc::Partial))?;
    out.pos(
        "partial-sources",
        "fixture_input(partial-missing-h1)+1 page",
        partial.clone(),
        "families partial with reasons (analyzer_row_limit, sitemap_evidence_mismatch, link_limit); S04 unavailable (analysis_deadline)",
    );

    let omissions = gen_v2(&tiny, families(&tiny, Sc::Omissions))?;
    out.pos(
        "capture-omissions",
        "scale(6 pages, 8 resources)",
        omissions.clone(),
        "collector_byte_limit and run_byte_limit are unknown with a reason, never pass; S08 unavailable (run_byte_limit)",
    );

    let big = site(150)?;
    out.pos(
        "family-item-limit",
        "site(150 pages)",
        gen_v2(&big, families(&big, Sc::ItemLimit))?,
        "S06 has 300 items against max_items 128: family_item_limit truncation",
    );
    out.pos(
        "family-byte-budget",
        "site(150 pages)",
        gen_v2(&big, families(&big, Sc::Heavy))?,
        "S03/S04/S05 items cut by the family byte allowance: family_byte_budget truncation",
    );
    let base = base_len(&big)?;
    let tight = base + RESERVE + 21 + 200_000;
    let degraded = {
        let mut r = big.clone();
        generate_wire_v2_with_cap(&mut r, &policy, heavy_everywhere(&big), tight)?
    };
    out.pos(
        "report-budget-degraded",
        "site(150 pages)",
        degraded,
        "producer cap tightened so the families budget (~200 KB) is under 786,432 B: budget_state report_byte_budget",
    );
    let limits = limits_site()?;
    out.pos(
        "wire2-limits",
        "limits-site(150 pages without titles)",
        gen_v2(&limits, families(&limits, Sc::Complete))?,
        "150 observations (> the wire-1 limit of 128) and more than 512 evaluations, untruncated under 1,024/1,024",
    );
    out.pos(
        "unbound-url-item",
        "scale(6 pages, 8 resources)",
        gen_v2(&tiny, families(&tiny, Sc::Unbound))?,
        "an item with a null locator and entity_id carries facts.url (sitemap entry not crawled)",
    );
    let rich = rich_site()?;
    let locators = gen_v2(&rich, families(&rich, Sc::Locators))?;
    out.pos(
        "locators-v2",
        "scale(1 page, 23 resources, 23 links, 23 relationships)",
        locators.clone(),
        "items at /pages/0/links/22 and /pages/0/relationships/22: the last link and relationship of page 0",
    );
    let mixed = mixed_caps()?;
    out.pos(
        "s14-record-limit",
        "mixed-caps(200 pages)",
        gen_v2(&mixed, families(&mixed, Sc::Complete))?,
        "S14 policy records capped at 512/128: per-rule omissions (record_limit, observation_limit); finding_coverage partial",
    );
    let ext = external_links_checked()?;
    out.pos(
        "external-links-checked-v2",
        "v3-local-complete (check_external_links)",
        gen_v2(&ext, families(&ext, Sc::Complete))?,
        "an out-of-scope entity and edge ids in the in-memory candidate lists; digest rows only on the wire",
    );
    let scale_report = scale::scale_report(scale::Shape::site(1696, 8))?;
    out.pos(
        "manifest-scale",
        "scale(57 pages, 1,696 resources, 8 external links per page)",
        gen_v2(&scale_report, families(&scale_report, Sc::Complete))?,
        "1,753 entities and 456 out-of-scope edges: the geometry that fails under wire-1 (policy_manifest_limit); delivered under wire-2",
    );
    let thousand = scale::scale_report(scale::Shape::site(967, 2))?;
    out.pos(
        "entities-1024",
        "scale(57 pages, 967 resources)",
        gen_v2(&thousand, families(&thousand, Sc::Complete))?,
        "exactly 1,024 entities (the analyzer row limit is 1,000): admit-time fixture",
    );
    let exact = padded_to(HTP_MAX_REPORT_BYTES)?;
    out.pos(
        "exact-cap-v2",
        "cap(60 pages, padded open_graph)",
        exact.clone(),
        "exactly 4,952,756 B (HTP_MAX_REPORT_BYTES), produced through generate_wire_v2_with_cap (the production budgeter keeps a 65,536 B reserve)",
    );

    // ---- v1 twins used as bases for the label negatives
    let v1_value = {
        let mut r = tiny.clone();
        parse(&generate_wire_v1(&mut r, &policy)?)?
    };

    // ---- negatives
    let v2: Value = parse(&complete)?;
    let v2_partial: Value = parse(&partial)?;
    let v2_omissions: Value = parse(&omissions)?;
    let v2_locators: Value = parse(&locators)?;
    let v2_unbound: Value = parse(&gen_v2(&tiny, families(&tiny, Sc::Unbound))?)?;
    let v2_limit: Value = parse(&gen_v2(&big, families(&big, Sc::ItemLimit))?)?;
    let shape = |e: &'static str| Expect::Reject("inspection", e);
    let rej_shape = shape("Shape");
    let rej_bounds = shape("Bounds");
    let tc = "complete-small";

    // families container
    {
        let mut v = v1_value.clone();
        v["evidence_families"] = Value::Null;
        out.neg(
            "families-null-v1",
            "v1 twin of complete-small",
            "s14-wire-1",
            &v,
            rej_shape,
            "evidence_families: null under the v1 label",
        )?;
        let mut v = v1_value.clone();
        v["evidence_families"] = v2["evidence_families"].clone();
        out.neg(
            "families-object-v1",
            "v1 twin of complete-small",
            "s14-wire-1",
            &v,
            rej_shape,
            "a valid families object under the v1 label",
        )?;
    }
    let neg2 = |out: &mut Out,
                name: &str,
                base: &'static str,
                from: &Value,
                expect: Expect,
                note: &'static str,
                f: &dyn Fn(&mut Value)|
     -> Result<()> {
        let mut v = from.clone();
        f(&mut v);
        out.neg(name, base, "s14-wire-2", &v, expect, note)
    };
    neg2(
        &mut out,
        "families-null-v2",
        tc,
        &v2,
        rej_shape,
        "evidence_families: null under v2",
        &|v| {
            v["evidence_families"] = Value::Null;
        },
    )?;
    neg2(
        &mut out,
        "families-missing-v2",
        tc,
        &v2,
        rej_shape,
        "evidence_families absent under v2",
        &|v| {
            v.as_object_mut().unwrap().remove("evidence_families");
        },
    )?;
    neg2(
        &mut out,
        "families-not-object-v2",
        tc,
        &v2,
        rej_shape,
        "evidence_families is an array",
        &|v| {
            v["evidence_families"] = json!([]);
        },
    )?;
    neg2(
        &mut out,
        "families-unknown-top-key",
        tc,
        &v2,
        rej_shape,
        "unknown key beside version in evidence_families",
        &|v| {
            v["evidence_families"]["extra"] = json!(1);
        },
    )?;
    neg2(
        &mut out,
        "families-unknown-family-key",
        tc,
        &v2,
        rej_shape,
        "a seventh family",
        &|v| {
            let s = v["evidence_families"]["families"]["s02_indexing"].clone();
            v["evidence_families"]["families"]["s09_extra"] = s;
        },
    )?;
    neg2(
        &mut out,
        "families-missing-family",
        tc,
        &v2,
        rej_shape,
        "s08_resources absent",
        &|v| {
            v["evidence_families"]["families"]
                .as_object_mut()
                .unwrap()
                .remove("s08_resources");
        },
    )?;
    neg2(
        &mut out,
        "families-unknown-section-key",
        tc,
        &v2,
        rej_shape,
        "unknown key in a section",
        &|v| {
            section_mut(v, "s02_indexing")["extra"] = json!(1);
        },
    )?;
    neg2(
        &mut out,
        "families-unknown-coverage-key",
        tc,
        &v2,
        rej_shape,
        "unknown key in coverage",
        &|v| {
            section_mut(v, "s02_indexing")["coverage"]["extra"] = json!(1);
        },
    )?;
    neg2(
        &mut out,
        "families-unknown-item-key",
        tc,
        &v2,
        rej_shape,
        "unknown key in an item",
        &|v| {
            first_item_mut(v, "s02_indexing").unwrap()["extra"] = json!(1);
        },
    )?;
    neg2(
        &mut out,
        "families-bad-version",
        tc,
        &v2,
        shape("UnsupportedSchema"),
        "evidence_families.version 2",
        &|v| {
            v["evidence_families"]["version"] = json!(2);
        },
    )?;
    neg2(
        &mut out,
        "families-bad-budget-state",
        tc,
        &v2,
        rej_shape,
        "budget_state outside the two allowed values",
        &|v| {
            v["evidence_families"]["budget_state"] = json!("bogus");
        },
    )?;

    // section invariants (design 3.2)
    neg2(
        &mut out,
        "families-items-total-mismatch",
        tc,
        &v2,
        rej_shape,
        "invariant 1: items_total != items + omitted_items",
        &|v| {
            let s = section_mut(v, "s02_indexing");
            s["items_total"] = json!(s["items_total"].as_u64().unwrap() + 1);
        },
    )?;
    neg2(
        &mut out,
        "families-truncated-mismatch",
        tc,
        &v2,
        rej_shape,
        "invariant 2: truncated with no omitted items",
        &|v| {
            section_mut(v, "s02_indexing")["truncated"] = json!(true);
        },
    )?;
    neg2(
        &mut out,
        "families-omission-reason-missing",
        tc,
        &v2,
        rej_shape,
        "invariant 2: omitted items without omitted_reason",
        &|v| {
            let s = section_mut(v, "s02_indexing");
            s["omitted_items"] = json!(1);
            s["items_total"] = json!(s["items_total"].as_u64().unwrap() + 1);
            s["truncated"] = json!(true);
        },
    )?;
    neg2(
        &mut out,
        "families-complete-with-unknown",
        tc,
        &v2,
        rej_shape,
        "invariant 3: state complete with coverage.unknown 1",
        &|v| {
            let s = section_mut(v, "s02_indexing");
            s["coverage"]["unknown"] = json!(1);
            s["coverage"]["unknown_reasons"] = json!({"target_not_fetched": 1});
        },
    )?;
    neg2(
        &mut out,
        "families-unavailable-with-items",
        tc,
        &v2,
        rej_shape,
        "invariant 3: state unavailable with items",
        &|v| {
            let s = section_mut(v, "s02_indexing");
            s["state"] = json!("unavailable");
            s["reason"] = json!("analyzer_error");
        },
    )?;
    neg2(
        &mut out,
        "families-unknown-without-reason",
        "partial-sources",
        &v2_partial,
        rej_shape,
        "invariant 4: an unknown item with a null reason",
        &|v| {
            first_item_mut(v, "s02_indexing").unwrap()["reason"] = Value::Null;
        },
    )?;
    neg2(
        &mut out,
        "families-unknown-reasons-sum",
        "partial-sources",
        &v2_partial,
        rej_shape,
        "invariant 4: unknown_reasons does not sum to coverage.unknown",
        &|v| {
            let s = section_mut(v, "s02_indexing");
            let n = s["coverage"]["unknown_reasons"]["robots_unavailable"]
                .as_u64()
                .unwrap();
            s["coverage"]["unknown_reasons"]["robots_unavailable"] = json!(n + 1);
        },
    )?;
    neg2(
        &mut out,
        "families-pass-with-capture-omission",
        "capture-omissions",
        &v2_omissions,
        rej_shape,
        "invariant 4: a collector_byte_limit item reported as pass",
        &|v| {
            for item in section_mut(v, "s05_link_graph")["items"]
                .as_array_mut()
                .unwrap()
            {
                item["outcome"] = json!("pass");
            }
        },
    )?;
    neg2(
        &mut out,
        "families-locator-unresolved",
        tc,
        &v2,
        rej_shape,
        "invariant 5: locator /pages/9999 does not resolve",
        &|v| {
            first_item_mut(v, "s02_indexing").unwrap()["locator"] = json!("/pages/9999");
        },
    )?;
    neg2(
        &mut out,
        "families-locator-bad-form",
        tc,
        &v2,
        rej_shape,
        "invariant 5: locator outside the three forms",
        &|v| {
            first_item_mut(v, "s02_indexing").unwrap()["locator"] = json!("/page/1");
        },
    )?;
    neg2(
        &mut out,
        "families-entity-id-mismatch",
        tc,
        &v2,
        rej_shape,
        "invariant 5: entity_id of another page",
        &|v| {
            let other =
                v["evidence_families"]["families"]["s02_indexing"]["items"][1]["entity_id"].clone();
            first_item_mut(v, "s02_indexing").unwrap()["entity_id"] = other;
        },
    )?;
    neg2(
        &mut out,
        "families-null-locator-no-url",
        "unbound-url-item",
        &v2_unbound,
        rej_shape,
        "invariant 5: a null-locator item without facts.url",
        &|v| {
            first_item_mut(v, "s03_integrity").unwrap()["facts"] = json!({});
        },
    )?;
    neg2(
        &mut out,
        "families-link-locator-out-of-range",
        "locators-v2",
        &v2_locators,
        rej_shape,
        "invariant 5: /pages/0/links/23 on a page with 23 links",
        &|v| {
            let items = section_mut(v, "s05_link_graph")["items"]
                .as_array_mut()
                .unwrap();
            for item in items {
                if item["locator"] == "/pages/0/links/22" {
                    item["locator"] = json!("/pages/0/links/23");
                }
            }
        },
    )?;
    neg2(
        &mut out,
        "families-relationship-locator-out-of-range",
        "locators-v2",
        &v2_locators,
        rej_shape,
        "invariant 5: /pages/0/relationships/23 on a page with 23",
        &|v| {
            let items = section_mut(v, "s05_link_graph")["items"]
                .as_array_mut()
                .unwrap();
            for item in items {
                if item["locator"] == "/pages/0/relationships/22" {
                    item["locator"] = json!("/pages/0/relationships/23");
                }
            }
        },
    )?;
    neg2(
        &mut out,
        "families-facts-too-big",
        tc,
        &v2,
        rej_bounds,
        "invariant 6: facts of more than 4,096 serialized bytes",
        &|v| {
            first_item_mut(v, "s02_indexing").unwrap()["facts"] =
                json!({"a": "x".repeat(1_500), "b": "x".repeat(1_500), "c": "x".repeat(1_500)});
        },
    )?;
    neg2(
        &mut out,
        "families-facts-too-deep",
        tc,
        &v2,
        rej_bounds,
        "invariant 6: facts nested to depth 5 (facts itself is level 1)",
        &|v| {
            first_item_mut(v, "s02_indexing").unwrap()["facts"] =
                json!({"a": {"b": {"c": {"d": {"e": 1}}}}});
        },
    )?;
    neg2(
        &mut out,
        "families-items-over-max",
        "family-item-limit",
        &v2_limit,
        rej_bounds,
        "invariant 6: 129 items in a section with max_items 128",
        &|v| {
            let s = section_mut(v, "s06_duplicates");
            let mut extra = s["items"].as_array().unwrap().last().unwrap().clone();
            let kind = format!("{}z", extra["kind"].as_str().unwrap());
            extra["kind"] = json!(kind);
            s["items"].as_array_mut().unwrap().push(extra);
            s["omitted_items"] = json!(s["omitted_items"].as_u64().unwrap() - 1);
        },
    )?;
    neg2(
        &mut out,
        "families-unsorted",
        tc,
        &v2,
        rej_shape,
        "invariant 7: the first two items of a section swapped",
        &|v| {
            section_mut(v, "s02_indexing")["items"]
                .as_array_mut()
                .unwrap()
                .swap(0, 1);
            // The swap must change the order: the items differ in locator.
        },
    )?;
    neg2(
        &mut out,
        "families-wrong-max-bytes",
        tc,
        &v2,
        rej_shape,
        "a section max_bytes that differs from the table",
        &|v| {
            section_mut(v, "s02_indexing")["max_bytes"] = json!(1);
        },
    )?;

    // manifest rows (A9)

    neg2(
        &mut out,
        "manifest-list-under-v2",
        tc,
        &v2,
        rej_bounds,
        "a manifest row still carrying candidates",
        &|v| {
            rows(v)[0]["candidates"] = json!([]);
        },
    )?;
    neg2(
        &mut out,
        "manifest-row-missing-digest",
        tc,
        &v2,
        rej_bounds,
        "a row without candidates_digest",
        &|v| {
            rows(v)[0]
                .as_object_mut()
                .unwrap()
                .remove("candidates_digest");
        },
    )?;
    neg2(
        &mut out,
        "manifest-row-bad-digest",
        tc,
        &v2,
        rej_bounds,
        "a malformed candidates_digest",
        &|v| {
            rows(v)[0]["candidates_digest"] = json!("sha256:zz");
        },
    )?;
    neg2(
        &mut out,
        "manifest-row-extra-key",
        tc,
        &v2,
        rej_bounds,
        "an unknown key in a manifest row",
        &|v| {
            rows(v)[0]["extra"] = json!(1);
        },
    )?;
    neg2(
        &mut out,
        "manifest-over-16384-v2",
        tc,
        &v2,
        rej_bounds,
        "a v2 manifest of more than 16,384 compact bytes",
        &|v| {
            for row in rows(v) {
                row["rule_id"] = json!("x".repeat(1_400));
            }
        },
    )?;
    neg2(
        &mut out,
        "s01-obs-1025-v2",
        tc,
        &v2,
        rej_bounds,
        "evidence_contract with 1,025 observations",
        &|v| {
            let obs = v["evidence_contract"]["observations"]
                .as_array_mut()
                .unwrap();
            let first = obs[0].clone();
            while obs.len() < 1_025 {
                obs.push(first.clone());
            }
        },
    )?;
    neg2(
        &mut out,
        "s01-evals-1025-v2",
        tc,
        &v2,
        rej_bounds,
        "evidence_contract with 1,025 evaluations",
        &|v| {
            let ev = v["evidence_contract"]["evaluations"]
                .as_array_mut()
                .unwrap();
            let first = ev[0].clone();
            while ev.len() < 1_025 {
                ev.push(first.clone());
            }
        },
    )?;
    {
        let mut v = v1_value.clone();
        v["policy_guidance"]["version"] = json!("s14-wire-3");
        out.neg(
            "unknown-wire-label",
            "v1 twin of complete-small",
            "s14-wire-3",
            &v,
            rej_bounds,
            "label s14-wire-3, no families: the label is the only defect",
        )?;
    }
    neg2(
        &mut out,
        "unknown-wire-label-with-families",
        tc,
        &v2,
        rej_shape,
        "evidence_families under an unrecognised label",
        &|v| {
            v["policy_guidance"]["version"] = json!("s14-wire-3");
        },
    )?;

    // summary stage: the receiver rebuilds the lists and compares
    let sm = |e: &'static str| Expect::Reject("summary", e);
    neg2(
        &mut out,
        "manifest-count-mismatch",
        tc,
        &v2,
        sm("SemanticMismatch"),
        "candidates_count off by one",
        &|v| {
            let n = rows(v)[0]["candidates_count"].as_u64().unwrap();
            rows(v)[0]["candidates_count"] = json!(n + 1);
        },
    )?;
    neg2(
        &mut out,
        "manifest-count-below-emitted",
        tc,
        &v2,
        sm("SemanticMismatch"),
        "candidates_count below the emitted record count",
        &|v| {
            let at = rows(v)
                .iter()
                .position(|r| r["candidates_count"].as_u64().unwrap_or(0) > 0)
                .expect("a row with candidates");
            rows(v)[at]["candidates_count"] = json!(0);
        },
    )?;
    neg2(
        &mut out,
        "manifest-digest-mismatch",
        tc,
        &v2,
        sm("SemanticMismatch"),
        "candidates_digest of a different list",
        &|v| {
            rows(v)[0]["candidates_digest"] =
                json!(digest_of(&["sha256:not-a-candidate".to_owned()]));
        },
    )?;
    {
        // Digest rows under the v1 label: the v1 receiver reads `candidates` and fails at summary.
        let mut v = v1_value.clone();
        for row in v["policy_guidance"]["manifest"].as_array_mut().unwrap() {
            let ids: Vec<String> = serde_json::from_value(row["candidates"].clone())?;
            let o = row.as_object_mut().unwrap();
            o.remove("candidates");
            o.insert("candidates_count".into(), json!(ids.len()));
            o.insert("candidates_digest".into(), json!(digest_of(&ids)));
        }
        out.neg(
            "manifest-digest-under-v1",
            "v1 twin of complete-small",
            "s14-wire-1",
            &v,
            sm("SemanticMismatch"),
            "digest-form rows under the v1 label",
        )?;
    }

    // population stage
    neg2(
        &mut out,
        "population-duplicate-finding-v2",
        "partial-sources",
        &v2_partial,
        Expect::Reject("population", "SemanticMismatch"),
        "a finding present twice: multiplicity differs from the rebuilt population",
        &|v| {
            let f = v["findings"].as_array_mut().unwrap();
            let first = f[0].clone();
            f.push(first);
        },
    )?;

    // one byte over the HTP cap: the exact-cap positive with one more padding character
    {
        let mut v = parse(&exact)?;
        let pad = v["pages"][0]["open_graph"]["og:pad0"][0]
            .as_str()
            .context("pad value")?
            .to_owned();
        v["pages"][0]["open_graph"]["og:pad0"][0] = json!(format!("{pad}x"));
        let bytes = ser(&v)?;
        ensure!(
            bytes.len() == HTP_MAX_REPORT_BYTES + 1,
            "cap-plus-one is not one byte over"
        );
        out.cases.push(Case {
            path: "negative/cap-plus-one-v2.json".into(),
            base: "exact-cap-v2",
            wire: "s14-wire-2",
            bytes,
            expect: Expect::Reject("inspection", "ByteLimit"),
            note: "4,952,757 B: one byte over HTP_MAX_REPORT_BYTES (the API answers 413 report_limit before admit; admit itself says ByteLimit)",
        });
    }
    Ok(out.cases)
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The state of each family of a v2 document, for the manifest.
fn family_states(doc: &Value) -> Value {
    let mut out = Map::new();
    if let Some(fams) = doc["evidence_families"]["families"].as_object() {
        for (k, s) in fams {
            out.insert(k.clone(), s["state"].clone());
        }
    }
    Value::Object(out)
}

pub fn manifest(cases: &[Case]) -> Result<Value> {
    let mut files = Vec::new();
    for c in cases {
        let doc: Value = serde_json::from_slice(&c.bytes)?;
        let expect = match c.expect {
            Expect::Admit(rev) => json!({"admit": true, "validator_revision": rev}),
            Expect::Reject(stage, error) => json!({"admit": false, "stage": stage, "error": error}),
        };
        let mut entry = json!({
            "path": c.path,
            "bytes": c.bytes.len(),
            "sha256": sha_hex(&c.bytes),
            "wire": c.wire,
            "base": c.base,
            "note": c.note,
            "expect": expect,
        });
        if matches!(c.expect, Expect::Admit(_)) {
            entry["entities"] = json!(doc["pages"].as_array().map_or(0, Vec::len));
            entry["families"] = family_states(&doc);
        }
        files.push(entry);
    }
    Ok(json!({
        "manifest_version": 1,
        "contract": "docs/contracts/s14-wire-2.md",
        "generator": "examples/make_wire2_fixtures.rs",
        "max_report_bytes": HTP_MAX_REPORT_BYTES,
        "v1_revision": V1_REVISION,
        "v2_revision": V2_REVISION,
        "note": "Generated only by the Rust producer; never hand-edited. 'expect' is the design section 5 stage table; scripts/verify-s14-schema4.py --wire2 --admit <bin> compares it with the HTP receiver's `rejected stage=.. error=..` line. No fixture expects the commit stage (SQL-only rejections are tested in 8c-2).",
        "files": files,
    }))
}

pub fn write_all(dir: &std::path::Path) -> Result<Vec<Case>> {
    let cases = cases()?;
    for sub in ["positive", "negative"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    for c in &cases {
        std::fs::write(dir.join(&c.path), &c.bytes)?;
    }
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest(&cases)?)? + "\n",
    )?;
    Ok(cases)
}

fn main() -> Result<()> {
    let dir = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_DIR.into()),
    );
    let cases = write_all(&dir)?;
    for c in &cases {
        println!("{} {} B", c.path, c.bytes.len());
    }
    if cases.iter().any(|c| c.bytes.is_empty()) {
        bail!("an empty fixture");
    }
    Ok(())
}
