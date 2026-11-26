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

fn sitemap_origin_site(entries: &[&str]) -> Site {
    let urls: String = entries
        .iter()
        .map(|u| format!("<url><loc>https://site.invalid{u}</loc></url>"))
        .collect();
    Site::default()
        .robots("User-agent: *\nAllow: /\nSitemap: https://site.invalid/sitemap.xml\n")
        .raw(
            "https://site.invalid/sitemap.xml",
            200,
            &[("content-type", "application/xml")],
            format!("<urlset>{urls}</urlset>").as_bytes(),
        )
        .html("https://site.invalid/", &page(&["/a"]))
        .html("https://site.invalid/a", &page(&[]))
        .html("https://site.invalid/orphan", &page(&[]))
}

/// A wire2 job that resumed with a changed sitemap keeps the frontier's sticky
/// `in_sitemap` flag for a page the retained sitemap no longer lists. The families
/// output must say so: S03 is `partial` with a named reason, the page's item is
/// `unknown` (never a pass or an issue), and the summary counts the mismatch.
#[tokio::test]
async fn resumed_job_with_a_changed_sitemap_reports_the_mismatch_in_s03() {
    let dir = tempfile::tempdir().unwrap();
    let store = seosmith::store::Store::open(dir.path().join("s.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let r = support::site::request(10, 0);
    store.submit(&r).await.unwrap();
    store.claim_with(CaptureMode::Wire2).await.unwrap().unwrap();
    seosmith::crawler::crawl_captured_with(
        &store,
        &support::site::unpaced(&r),
        CaptureMode::Wire2,
        &sitemap_origin_site(&["/", "/a", "/orphan"]),
    )
    .await
    .unwrap();
    store.recover().await.unwrap();
    store.claim_with(CaptureMode::None).await.unwrap().unwrap();
    let id = r.request_id.to_string();
    let stored = store.capture_mode(&id).await.unwrap();
    let report = seosmith::crawler::crawl_captured_with(
        &store,
        &support::site::unpaced(&r),
        stored,
        &sitemap_origin_site(&["/", "/a"]),
    )
    .await
    .unwrap();
    let set = families::build_all(&store, &id, &report, stored, &SystemClock).await;
    let job = support::Job {
        _dir: dir,
        store,
        id,
        report,
    };
    check_invariants(&set, &job.report);
    let s03 = section(&set, FamilyKey::S03);
    assert_eq!(s03.state, SectionState::Partial, "{s03:#?}");
    assert_eq!(s03.reason.as_deref(), Some("sitemap_evidence_mismatch"));
    assert_eq!(s03.summary["sitemap_flag_mismatch_pages"], 1);
    let mismatched: Vec<_> = s03
        .items
        .iter()
        .filter(|i| i.kind == "sitemap_origin_unavailable")
        .collect();
    assert_eq!(mismatched.len(), 1, "{:?}", s03.items);
    assert_eq!(mismatched[0].outcome, Outcome::Unknown);
    let orphan = job
        .report
        .pages
        .iter()
        .position(|p| p.url == "https://site.invalid/orphan")
        .unwrap();
    assert_eq!(
        mismatched[0].locator.as_deref(),
        Some(format!("/pages/{orphan}").as_str())
    );
    // Pages the retained sitemap still lists get their normal origin item.
    assert!(
        s03.items
            .iter()
            .any(|i| i.kind == "sitemap_origin" && i.outcome != Outcome::Issue)
    );
}

/// R1: a page whose links were truncated has unknown outgoing edges too, so another
/// page's depth is only an upper bound and `deep_page` is `unknown`, never an issue.
#[tokio::test]
async fn s05_deep_page_is_unknown_when_another_page_is_link_truncated() {
    let site = Site::default()
        .robots("User-agent: *\nAllow: /\n")
        .html("https://site.invalid/", &page(&["/c1", "/x"]))
        .html("https://site.invalid/c1", &page(&["/c2"]))
        .html("https://site.invalid/c2", &page(&["/c3"]))
        .html("https://site.invalid/c3", &page(&["/c4"]))
        .html("https://site.invalid/c4", &page(&[]))
        .html("https://site.invalid/x", &page(&[]));
    let job = crawl(&site, 20, 0, CaptureMode::Wire2).await;
    allow_follow(&job).await;
    let deep = |set: &families::types::FamilySet| -> Vec<(Outcome, Option<String>)> {
        section(set, FamilyKey::S05)
            .items
            .iter()
            .filter(|i| i.kind == "deep_page")
            .map(|i| (i.outcome, i.reason.clone()))
            .collect()
    };
    let full = build(&job).await;
    assert_eq!(deep(&full), vec![(Outcome::Issue, None)]);
    // Mark /x's extracted links as truncated.
    let raw: String =
        sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=? AND url=?")
            .bind(&job.id)
            .bind("https://site.invalid/x")
            .fetch_one(&job.store.0)
            .await
            .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    for c in v["capture"]["collectors"].as_array_mut().unwrap() {
        if c["collector"] == "graph" {
            c["state"] = "partial".into();
        }
    }
    sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
        .bind(serde_json::to_string(&v).unwrap())
        .bind(&job.id)
        .bind("https://site.invalid/x")
        .execute(&job.store.0)
        .await
        .unwrap();
    let set = build(&job).await;
    check_invariants(&set, &job.report);
    assert_eq!(
        deep(&set),
        vec![(Outcome::Unknown, Some("link_limit".into()))],
        "{:?}",
        section(&set, FamilyKey::S05).items
    );
}
