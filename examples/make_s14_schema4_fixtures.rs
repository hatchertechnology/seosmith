//! Generates the S14 schema-4 paired fixtures under
//! `fixtures/contracts/s14-schema4-v1/` with the production serialiser
//! (`finding_coverage_prototype::generate_wire_v1`): raw `audit::analyze`
//! output, policy applied, evidence over the final findings, then
//! `finding_coverage`. Every file is the exact byte string the producer stores
//! and delivers.
//!
//! Usage (from the repository root):
//!   cargo +1.98.1 build --locked --features local-cli-fixtures --bin seosmith
//!   cargo +1.98.1 run --locked --features local-cli-fixtures \
//!       --example `make_s14_schema4_fixtures` [output-dir]
//!
//! The crawl case runs the real `seosmith crawl --policy-wire-v1` binary
//! against an offline fixture transport; set `SEOSMITH_BIN` to override the
//! binary location (default: next to the example under `target/<profile>/`).

use anyhow::{Context, Result, bail, ensure};
use seosmith::{
    delivery::HTP_MAX_REPORT_BYTES,
    finding_coverage_prototype::{fixture_input, generate_wire_v1},
    model::{Page, Report},
    policy::AdvisoryPolicy,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

const DEFAULT_DIR: &str = "fixtures/contracts/s14-schema4-v1";
/// The largest valid report is `HTP_MAX_REPORT_BYTES` (4,952,756) bytes.
const EXACT_CAP: usize = 4_952_756;

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

fn complete_base(path: &str) -> Result<Report> {
    let full = format!("{}/fixtures/policies/{path}", env!("CARGO_MANIFEST_DIR"));
    let mut report: Report = serde_json::from_str(&std::fs::read_to_string(&full)?)?;
    report.schema_version = 1;
    report.policy_guidance = None;
    report.evidence_contract = None;
    report.findings.clear();
    Ok(report)
}

fn external_links_checked() -> Result<Report> {
    // v3-local-complete: an internal page links to an external resource that
    // was fetched (check_external_links = true).
    complete_base("v3-local-complete.json")
}

fn mixed_caps_exceeded() -> Result<Report> {
    // 200 pages, most with an empty title: past MAX_OBSERVATIONS (128) and
    // MAX_EVALUATIONS (512), so evidence_contract.truncated is true. A few
    // pages are not observable (404, non-HTML, robots) to mix source states.
    let mut pages = Vec::new();
    for i in 0..200 {
        let url = format!("https://example.com/p{i}");
        let mut p = page(
            &url,
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

fn unicode() -> Result<Report> {
    let pages = vec![
        page(
            "https://example.com/",
            "Caf\u{e9} and \u{65e5}\u{672c}\u{8a9e} title",
        ),
        page("https://example.com/emoji", "\u{1F600}\u{1F600}"),
        page("https://example.com/combining", "e\u{301}e\u{301}e\u{301}"),
        page(
            "https://example.com/%E6%97%A5%E6%9C%AC",
            "Percent-encoded path",
        ),
        page("https://example.com/empty", ""),
        page("https://example.com/long", &"\u{1F600}".repeat(70)),
    ];
    with_pages("complete-clean-h1", pages)
}

/// Pads one `open_graph` value per page so the final serialisation is exactly
/// `target` bytes. Returns the bytes and the page count used.
fn exact_cap(policy: &AdvisoryPolicy, target: usize) -> Result<Vec<u8>> {
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
        let mut report = with_pages("complete-clean-h1", pages)?;
        generate_wire_v1(&mut report, policy)
    };
    let mut pads = vec![12_000; PAGES * KEYS];
    for _ in 0..4 {
        let len = build(&pads)?.len();
        if len == target {
            return build(&pads);
        }
        // ASCII padding adds exactly one byte per character.
        let mut grow = target.saturating_sub(len);
        let mut shrink = len.saturating_sub(target);
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
        let delta = grow + shrink;
        ensure!(delta == 0, "cannot reach the exact cap with {PAGES} pages");
    }
    let bytes = build(&pads)?;
    ensure!(bytes.len() == target, "exact-cap tuning did not converge");
    Ok(bytes)
}

/// Runs the real CLI against an offline fixture site and returns its stdout.
fn crawl_case(dir: &std::path::Path) -> Result<Vec<u8>> {
    let bin = match std::env::var_os("SEOSMITH_BIN") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()?
            .parent()
            .and_then(|p| p.parent())
            .context("no target dir")?
            .join("seosmith"),
    };
    ensure!(
        bin.exists(),
        "{} missing: build it with --features local-cli-fixtures --bin seosmith",
        bin.display()
    );
    let site = "https://fixture.invalid";
    let mut responses: BTreeMap<String, Value> = BTreeMap::new();
    responses.insert(
        format!("{site}/robots.txt"),
        json!({"status": 404, "body": ""}),
    );
    let pages = 24;
    let entries: Vec<String> = (1..pages)
        .map(|i| format!("<url><loc>{site}/page-{i}</loc></url>"))
        .collect();
    let sitemap = format!("<urlset>{}</urlset>", entries.concat());
    responses.insert(
        format!("{site}/sitemap.xml"),
        json!({"status": 200, "body": sitemap}),
    );
    let html = |title: &str, body: &str| {
        json!({"status": 200, "headers": {"content-type": "text/html"},
               "body": format!("<html lang=\"en\"><head><title>{title}</title></head><body>{body}</body></html>")})
    };
    let nav: String = (1..pages)
        .map(|i| format!("<a href=\"/page-{i}\">Page {i}</a>"))
        .collect::<Vec<_>>()
        .concat();
    responses.insert(
        format!("{site}/"),
        html("Fixture home page", &format!("<h1>Home</h1>{nav}")),
    );
    for i in 1..pages {
        let (title, body) = match i % 4 {
            0 => ("Hi".to_string(), "<h1>Short title</h1>".to_string()),
            1 => (String::new(), "<p>No title, no heading</p>".to_string()),
            2 => (
                format!("A well formed title for fixture page {i}"),
                format!(
                    "<h1>Page {i}</h1><a href=\"/page-{}\">next</a>",
                    i % (pages - 1) + 1
                ),
            ),
            _ => (
                format!("A well formed title for fixture page {i}"),
                "<p>Body without a heading</p>".to_string(),
            ),
        };
        responses.insert(format!("{site}/page-{i}"), html(&title, &body));
    }
    let request = json!({
        "request_id": "00000000-0000-4000-8000-0000000000c1",
        "website_id": "00000000-0000-4000-8000-000000000002",
        "binding_generation": 1,
        "url": format!("{site}/"),
        "max_pages": 100, "max_duration_seconds": 60, "delay_ms": 1000,
        "check_external_links": false, "max_resources": 0
    });
    let request_path = dir.join("request.json");
    let responses_path = dir.join("responses.json");
    std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
    std::fs::write(&responses_path, serde_json::to_vec(&responses)?)?;
    let output = Command::new(&bin)
        .env_clear()
        .env("SEOSMITH_LOCAL_FIXTURES", &responses_path)
        .args([
            "--database",
            dir.join("db.sqlite").to_str().context("db path")?,
        ])
        .args([
            "--advisory-policy",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fixtures/policies/production-default.json"
            ),
        ])
        .arg("--policy-wire-v1")
        .args([
            "crawl",
            "--request",
            request_path.to_str().context("request path")?,
        ])
        .output()?;
    if !output.status.success() {
        bail!("crawl failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    // Without --features local-cli-fixtures the fixture transport is compiled
    // out and the crawl fails over the network yet still exits 0.
    let doc: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        doc["outcome"] == "completed" && doc["pages"].as_array().is_some_and(|p| !p.is_empty()),
        "crawl did not complete (outcome {}); was the seosmith binary built with --features local-cli-fixtures?",
        doc["outcome"]
    );
    Ok(output.stdout)
}

fn main() -> Result<()> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_DIR.into()),
    );
    std::fs::create_dir_all(&out)?;
    let policy = AdvisoryPolicy::default();
    let mut cases: Vec<(&str, &str, Vec<u8>)> = Vec::new();
    let mut add = |name: &'static str, note: &'static str, report: Result<Report>| -> Result<()> {
        let mut report = report?;
        cases.push((
            name,
            note,
            generate_wire_v1(&mut report, &policy).with_context(|| name.to_string())?,
        ));
        Ok(())
    };
    add(
        "complete-clean",
        "complete audit, H1 present",
        fixture_input("complete-clean-h1"),
    )?;
    add(
        "complete-missing-h1",
        "complete audit, H1 missing",
        fixture_input("complete-missing-h1"),
    )?;
    add(
        "partial-sources",
        "partial audit: extraction truncated, page limit",
        fixture_input("partial-missing-h1"),
    )?;
    add(
        "external-links-unchecked",
        "external link present but not fetched",
        fixture_input("partial-clean-h1"),
    )?;
    add(
        "external-links-checked",
        "external resource fetched (check_external_links)",
        external_links_checked(),
    )?;
    add(
        "mixed-caps-exceeded",
        "200 pages, mixed source states, observation/evaluation caps exceeded",
        mixed_caps_exceeded(),
    )?;
    add(
        "unicode",
        "non-BMP, combining and percent-encoded inputs",
        unicode(),
    )?;
    let exact = exact_cap(&policy, EXACT_CAP)?;
    ensure!(exact.len() == HTP_MAX_REPORT_BYTES, "cap constant drifted");
    cases.push((
        "exact-cap",
        "largest valid report: exactly HTP_MAX_REPORT_BYTES",
        exact,
    ));
    let scratch = tempfile::tempdir()?;
    cases.push((
        "crawl",
        "real `seosmith crawl --policy-wire-v1` over an offline fixture site",
        crawl_case(scratch.path())?,
    ));

    let mut manifest = Vec::new();
    for (name, note, bytes) in &cases {
        let file = format!("{name}.json");
        std::fs::write(out.join(&file), bytes)?;
        let doc: Value = serde_json::from_slice(bytes)?;
        let evaluations = doc["evidence_contract"]["evaluations"]
            .as_array()
            .map_or(0, Vec::len);
        manifest.push(json!({
            "path": file,
            "note": note,
            "bytes": bytes.len(),
            "sha256": format!("{:x}", Sha256::digest(bytes)),
            "schema_version": doc["schema_version"],
            "findings": doc["findings"].as_array().map_or(0, Vec::len),
            "evaluations": evaluations,
            "evidence_truncated": doc["evidence_contract"]["truncated"],
            "finding_coverage_audit": doc["finding_coverage"]["audit"],
        }));
    }
    let manifest = json!({
        "status": "local-only-not-frozen",
        "note": "Generated by examples/make_s14_schema4_fixtures.rs with generate_wire_v1. Receiver admit results are recorded by scripts/verify-s14-schema4.py --admit.",
        "max_report_bytes": HTP_MAX_REPORT_BYTES,
        "files": manifest,
    });
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    Ok(())
}
