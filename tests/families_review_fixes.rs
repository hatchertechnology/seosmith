#![allow(clippy::format_collect)]
//! Review 8b2-o fixes F3, F4 and F6, each with a test that failed before the fix.
use seosmith::families::{
    self,
    types::{CaptureMode, Clock, FamilyKey, Outcome, SectionState, SystemClock},
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
#[path = "support/families.rs"]
mod support;
use support::{Site, check_invariants, crawl, rich_site, section};

/// The first call (the start of the run) is `base`; every later call is 1 µs short
/// of the 30 s total deadline. The total is not yet expired, but nothing is left.
struct NearlyOutOfTime {
    base: Instant,
    calls: AtomicU64,
}
impl Clock for NearlyOutOfTime {
    fn now(&self) -> Instant {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.base
        } else {
            self.base + Duration::from_micros(29_999_999)
        }
    }
}

#[tokio::test]
async fn analyzers_are_bounded_by_the_remaining_deadline() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    let clock = NearlyOutOfTime {
        base: Instant::now(),
        calls: AtomicU64::new(0),
    };
    let set =
        families::build_all(&job.store, &job.id, &job.report, CaptureMode::Wire2, &clock).await;
    check_invariants(&set, &job.report);
    for key in [
        FamilyKey::S02,
        FamilyKey::S03,
        FamilyKey::S05,
        FamilyKey::S08,
    ] {
        let s = section(&set, key);
        assert_eq!(
            (s.state, s.reason.as_deref()),
            (SectionState::Unavailable, Some("analysis_deadline")),
            "{}",
            key.as_str()
        );
    }
}

async fn drop_graph(job: &support::Job, url: &str) {
    let raw: String =
        sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=? AND url=?")
            .bind(&job.id)
            .bind(url)
            .fetch_one(&job.store.0)
            .await
            .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let c = v["capture"]["collectors"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["collector"] == "graph")
        .unwrap();
    c["state"] = "unavailable".into();
    c["reason"] = "collector_byte_limit".into();
    c["data"] = serde_json::Value::Null;
    sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
        .bind(serde_json::to_string(&v).unwrap())
        .bind(&job.id)
        .bind(url)
        .execute(&job.store.0)
        .await
        .unwrap();
}

async fn build(job: &support::Job) -> families::types::FamilySet {
    families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await
}

/// The in-memory origin gives no raw header evidence, so every page's followability is
/// `Unknown`. Mark every page followable so that the graph can establish distances.
async fn allow_follow(job: &support::Job) {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT url,evidence FROM local_page_evidence WHERE job_id=?")
            .bind(&job.id)
            .fetch_all(&job.store.0)
            .await
            .unwrap();
    for (url, raw) in rows {
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        for c in v["capture"]["collectors"].as_array_mut().unwrap() {
            if c["collector"] == "effective_indexing" {
                for d in c["data"]["decisions"].as_array_mut().unwrap() {
                    d["directives"]["follow"] = "Allowed".into();
                }
            }
        }
        sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
            .bind(serde_json::to_string(&v).unwrap())
            .bind(&job.id)
            .bind(&url)
            .execute(&job.store.0)
            .await
            .unwrap();
    }
}

fn page(links: &[&str]) -> String {
    let a: String = links
        .iter()
        .map(|l| format!("<a href='{l}'>l</a>"))
        .collect();
    format!(
        "<html><head><title>T</title><meta name='robots' content='index, follow'></head><body><h1>T</h1>{a}</body></html>"
    )
}

#[tokio::test]
async fn s05_deep_page_is_unknown_when_another_graph_is_missing() {
    let site = Site::default()
        .robots("User-agent: *\nAllow: /\nSitemap: https://site.invalid/sitemap.xml\n")
        .raw(
            "https://site.invalid/sitemap.xml",
            200,
            &[("content-type", "application/xml")],
            b"<urlset><url><loc>https://site.invalid/</loc></url></urlset>",
        )
        .html("https://site.invalid/", &page(&["/c1", "/x"]))
        .html("https://site.invalid/c1", &page(&["/c2"]))
        .html("https://site.invalid/c2", &page(&["/c3"]))
        .html("https://site.invalid/c3", &page(&["/c4"]))
        .html("https://site.invalid/c4", &page(&[]))
        .html("https://site.invalid/x", &page(&[]));
    let job = crawl(&site, 20, 10, CaptureMode::Wire2).await;
    allow_follow(&job).await;
    let kinds = |set: &families::types::FamilySet| -> Vec<(String, Outcome)> {
        section(set, FamilyKey::S05)
            .items
            .iter()
            .filter(|i| i.kind == "deep_page" || i.kind == "reachability")
            .map(|i| (i.kind.clone(), i.outcome))
            .collect()
    };
    // Sanity: with every graph present, /c4 is a deep issue.
    let full = build(&job).await;
    assert!(
        kinds(&full).contains(&("deep_page".into(), Outcome::Issue)),
        "{:?}",
        section(&full, FamilyKey::S05).items
    );
    drop_graph(&job, "https://site.invalid/x").await;
    let set = build(&job).await;
    check_invariants(&set, &job.report);
    let s05 = section(&set, FamilyKey::S05);
    assert!(
        s05.items
            .iter()
            .all(|i| !(i.kind == "deep_page" && i.outcome == Outcome::Issue)),
        "{:?}",
        s05.items
    );
    assert!(s05.items.iter().any(|i| i.kind == "deep_page"
        && i.outcome == Outcome::Unknown
        && i.reason.as_deref() == Some("collector_byte_limit")));
}

#[tokio::test]
async fn s08_never_passes_a_resource_with_unknown_dimensions() {
    let site = Site::default()
        .robots("User-agent: *\nAllow: /\n")
        .html(
            "https://site.invalid/",
            "<html><head><title>T</title></head><body><h1>T</h1><img src='/known.png'><img src='/bare.png'></body></html>",
        )
        .raw("https://site.invalid/known.png", 200, &[("content-type", "image/png"), ("cache-control", "max-age=3600")], &[1u8; 100])
        .raw("https://site.invalid/bare.png", 200, &[("content-type", "image/png")], &[2u8; 100]);
    let job = crawl(&site, 20, 10, CaptureMode::Wire2).await;
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    check_invariants(&set, &job.report);
    let s08 = section(&set, FamilyKey::S08);
    let n = |url: &str| {
        job.report
            .pages
            .iter()
            .position(|p| p.url == url)
            .map(|n| format!("/pages/{n}"))
            .unwrap()
    };
    let (known, bare) = (
        n("https://site.invalid/known.png"),
        n("https://site.invalid/bare.png"),
    );
    let has = |loc: &str, kind: &str, o: Outcome| {
        s08.items
            .iter()
            .any(|i| i.locator.as_deref() == Some(loc) && i.kind == kind && i.outcome == o)
    };
    assert!(
        has(&known, "resource_delivery", Outcome::Pass),
        "{:?}",
        s08.items
    );
    assert!(has(&bare, "cache_unverified", Outcome::Unknown));
    assert!(
        !has(&bare, "resource_delivery", Outcome::Pass),
        "{:?}",
        s08.items
    );
}
