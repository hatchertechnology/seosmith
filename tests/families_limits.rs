#![allow(clippy::format_push_string)]
//! Deadlines, item limits and the analyzer row limit.
use seosmith::families::{
    self,
    common::Ctx,
    s04,
    types::{CaptureMode, Clock, FamilyKey, SectionState, SystemClock},
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
#[path = "support/families.rs"]
mod support;
use support::{Site, check_invariants, crawl, rich_site, section};

/// Every call to `now` moves the clock forward by `step`.
struct StepClock {
    base: Instant,
    calls: AtomicU64,
    step: Duration,
}
impl StepClock {
    fn new(step: Duration) -> Self {
        Self {
            base: Instant::now(),
            calls: AtomicU64::new(0),
            step,
        }
    }
}
impl Clock for StepClock {
    fn now(&self) -> Instant {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.base + self.step * u32::try_from(n).unwrap()
    }
}

#[tokio::test]
async fn an_exhausted_total_deadline_makes_every_family_unavailable() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    let clock = StepClock::new(Duration::from_secs(31));
    let set =
        families::build_all(&job.store, &job.id, &job.report, CaptureMode::Wire2, &clock).await;
    check_invariants(&set, &job.report);
    for key in FamilyKey::ALL {
        let s = section(&set, key);
        assert_eq!(
            (s.state, s.reason.as_deref()),
            (SectionState::Unavailable, Some("analysis_deadline")),
            "{key:?}"
        );
    }
}

#[tokio::test]
async fn a_family_deadline_mid_family_keeps_what_was_built_and_counts_the_rest() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    // Ctx::new takes the start; the 11 s step puts every later check past the 10 s family budget.
    let clock = StepClock::new(Duration::from_secs(11));
    let ctx = Ctx::new(
        &job.report,
        std::collections::HashMap::new(),
        &clock,
        CaptureMode::Wire2,
    );
    let started = clock.base; // the family began before the clock advanced
    let s = s04::build(&ctx, &job.report, started);
    assert!(s.truncated && s.omitted_items > 0);
    assert_eq!(s.omitted_reason.as_deref(), Some("analysis_deadline"));
    assert_eq!(s.state, SectionState::Partial);
    assert_eq!(s.items_total, s.items.len() as u64 + s.omitted_items);
}

#[tokio::test]
async fn items_beyond_the_family_limit_are_counted_not_dropped_silently() {
    // 300 in-scope pages, each a soft-404-clear pass item: more than S06's 128 items.
    let mut site = Site::default().robots("User-agent: *\nAllow: /\n");
    let mut home = String::from("<html><head><title>H</title></head><body><h1>H</h1>");
    for n in 0..300 {
        home.push_str(&format!("<a href='/p{n}'>p{n}</a>"));
        site = site.html(&format!("https://site.invalid/p{n}"), &format!("<html><head><title>P{n}</title></head><body><h1>P{n}</h1><p>page {n} text</p></body></html>"));
    }
    home.push_str("</body></html>");
    let site = site.html("https://site.invalid/", &home);
    let job = crawl(&site, 400, 0, CaptureMode::Wire2).await;
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    check_invariants(&set, &job.report);
    let s06 = section(&set, FamilyKey::S06);
    assert_eq!(s06.items.len(), 128);
    assert!(s06.truncated);
    assert_eq!(s06.omitted_reason.as_deref(), Some("family_item_limit"));
    assert!(s06.items_total >= 301);
}
