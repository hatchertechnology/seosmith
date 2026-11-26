//! WP8b-2: `families::build_all` over a real worker-mode crawl.
use seosmith::families::{
    self,
    types::{CaptureMode, FamilyKey, Outcome, SectionState, SystemClock},
};
#[path = "support/families.rs"]
mod support;
use support::{check_invariants, crawl, rich_site, section};

#[tokio::test]
async fn rich_site_gives_six_valid_sections_with_expected_findings() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    check_invariants(&set, &job.report);
    let kinds = |key| -> Vec<(String, Outcome)> {
        section(&set, key)
            .items
            .iter()
            .map(|i| (i.kind.clone(), i.outcome))
            .collect()
    };
    let s02 = kinds(FamilyKey::S02);
    assert!(
        s02.contains(&("index_restricted".into(), Outcome::Issue)),
        "{s02:?}"
    );
    assert!(
        s02.contains(&("crawl_denied".into(), Outcome::Issue)),
        "{s02:?}"
    );
    let s03 = kinds(FamilyKey::S03);
    assert!(
        s03.iter().any(|(k, _)| k.starts_with("canonical_")
            || k.starts_with("hreflang_")
            || k.starts_with("sitemap_")),
        "{s03:?}"
    );
    let s05 = kinds(FamilyKey::S05);
    assert!(
        s05.contains(&("broken_fragment".into(), Outcome::Issue)),
        "{s05:?}"
    );
    let s06 = section(&set, FamilyKey::S06);
    assert!(
        s06.items
            .iter()
            .any(|i| i.kind == "exact_duplicate_group" && i.facts["member_count"] == 2)
    );
    assert_eq!(
        s06.summary["near_duplicate"]["reason"],
        "main_text_not_retained"
    );
    assert!(
        s06.coverage
            .unknown_reasons
            .contains_key("main_text_not_retained")
    );
    assert_eq!(s06.state, SectionState::Partial);
    let s08 = kinds(FamilyKey::S08);
    assert!(
        s08.contains(&("uncompressed_resource".into(), Outcome::Issue)),
        "{s08:?}"
    );
    assert!(
        s08.contains(&("uncached_resource".into(), Outcome::Issue)),
        "{s08:?}"
    );
    // The text css has no proof of compression either way: unknown, never an issue.
    assert!(
        s08.contains(&("cache_unverified".into(), Outcome::Unknown)),
        "{s08:?}"
    );
}

#[tokio::test]
async fn structured_data_issues_are_reported_per_page() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    let s04 = section(&set, FamilyKey::S04);
    assert_eq!(
        s04.semantics,
        seosmith::structured_validation::PROFILE_VERSION
    );
    assert!(s04.summary["pages_with_structured_data"].as_u64().unwrap() >= 1);
    assert!(
        s04.items
            .iter()
            .any(|i| i.outcome == Outcome::Issue && i.facts["category"].is_string()),
        "{:?}",
        s04.items
    );
}

#[tokio::test]
async fn output_is_deterministic() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    let a = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    let b = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    assert_eq!(
        serde_json::to_vec(&a).unwrap(),
        serde_json::to_vec(&b).unwrap()
    );
}

#[tokio::test]
async fn a_job_captured_as_none_has_six_unavailable_sections() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::None).await;
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::None,
        &SystemClock,
    )
    .await;
    check_invariants(&set, &job.report);
    for key in FamilyKey::ALL {
        let s = section(&set, key);
        assert_eq!(s.state, SectionState::Unavailable);
        assert_eq!(s.reason.as_deref(), Some("capture_mode_changed"));
    }
}

#[tokio::test]
async fn an_unreadable_job_gives_analyzer_error_not_a_panic() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    sqlx::query("DROP TABLE local_robots_run")
        .execute(&job.store.0)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET execution_mode='local'")
        .execute(&job.store.0)
        .await
        .unwrap();
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
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
            (SectionState::Unavailable, Some("analyzer_error")),
            "{key:?}"
        );
    }
    // S04 and S06 do not need the saved-job readers.
    assert_ne!(
        section(&set, FamilyKey::S04).state,
        SectionState::Unavailable
    );
}

/// Empty 404s and redirects share one body digest: they are not duplicate content.
#[tokio::test]
async fn s06_only_groups_2xx_bodies() {
    use support::Site;
    let site = Site::default()
        .robots("User-agent: *\nAllow: /\n")
        .html(
            "https://site.invalid/",
            "<html><head><title>H</title></head><body><h1>H</h1><a href='/m1'>1</a><a href='/m2'>2</a><a href='/r1'>3</a><a href='/r2'>4</a><a href='/t1'>5</a><a href='/t2'>6</a></body></html>",
        )
        .raw("https://site.invalid/m1", 404, &[("content-type", "text/html")], b"")
        .raw("https://site.invalid/m2", 404, &[("content-type", "text/html")], b"")
        .raw("https://site.invalid/r1", 301, &[("content-type", "text/html")], b"")
        .raw("https://site.invalid/r2", 301, &[("content-type", "text/html")], b"")
        .raw("https://site.invalid/t1", 404, &[("content-type", "text/html")], b"<html><body>not found</body></html>")
        .raw("https://site.invalid/t2", 404, &[("content-type", "text/html")], b"<html><body>not found</body></html>");
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
    let s06 = section(&set, FamilyKey::S06);
    assert!(
        s06.items.iter().all(|i| i.kind != "exact_duplicate_group"),
        "{:?}",
        s06.items
    );
    assert_eq!(s06.summary["exact_duplicate_groups"], 0);
}
