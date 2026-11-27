#![allow(clippy::format_push_string)]
//! Over 1,000 entities the analyzers report their row limit, and building stays in memory budget.
use seosmith::families::{
    self,
    types::{CaptureMode, FamilyKey, SectionState, SystemClock},
};
#[path = "support/families.rs"]
mod support;
use support::{Site, check_invariants, crawl, section};

fn vm_hwm_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap()
}

#[tokio::test]
async fn over_a_thousand_entities_families_are_partial_and_fit_in_256_mib() {
    let mut site = Site::default().robots("User-agent: *\nAllow: /\n");
    let mut home = String::from("<html><head><title>H</title></head><body><h1>H</h1>");
    for h in 0..35 {
        home.push_str(&format!("<a href='/h{h}'>h{h}</a>"));
        let mut hub = format!("<html><head><title>Hub {h}</title></head><body><h1>Hub {h}</h1>");
        for l in 0..30 {
            hub.push_str(&format!("<a href='/h{h}/l{l}'>l{l}</a>"));
            site = site.html(&format!("https://site.invalid/h{h}/l{l}"), &format!("<html><head><title>L {h} {l}</title></head><body><h1>L</h1><p>leaf {h} {l}</p></body></html>"));
        }
        hub.push_str("</body></html>");
        site = site.html(&format!("https://site.invalid/h{h}"), &hub);
    }
    home.push_str("</body></html>");
    let site = site.html("https://site.invalid/", &home);
    let job = crawl(&site, 1100, 0, CaptureMode::Wire2).await;
    assert!(job.report.pages.len() > 1000, "{}", job.report.pages.len());
    // Reset the peak so the figure below covers the families build, not the crawl.
    std::fs::write("/proc/self/clear_refs", "5").unwrap();
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    let peak_mib = vm_hwm_kib() / 1024;
    println!(
        "peak RSS after build_all: {peak_mib} MiB at {} entities",
        job.report.pages.len()
    );
    assert!(peak_mib <= 256, "peak RSS {peak_mib} MiB");
    check_invariants(&set, &job.report);
    for key in [FamilyKey::S02, FamilyKey::S03, FamilyKey::S05] {
        let s = section(&set, key);
        assert_eq!(s.state, SectionState::Partial, "{key:?}");
        assert_eq!(s.reason.as_deref(), Some("analyzer_row_limit"), "{key:?}");
    }
}
