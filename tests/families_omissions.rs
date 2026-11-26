//! A collector dropped by a capture bound is `unknown` with that reason, never `pass`.
use seosmith::families::{
    self,
    types::{CaptureMode, FamilyKey, Outcome, SectionState, SystemClock},
};
#[path = "support/families.rs"]
mod support;
use support::{Job, check_invariants, crawl, rich_site, section};

async fn drop_collector(job: &Job, url: &str, collector: &str, reason: &str) {
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
        .find(|c| c["collector"] == collector)
        .unwrap();
    c["state"] = "unavailable".into();
    c["reason"] = reason.into();
    c["data"] = serde_json::Value::Null;
    sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
        .bind(serde_json::to_string(&v).unwrap())
        .bind(&job.id)
        .bind(url)
        .execute(&job.store.0)
        .await
        .unwrap();
}

const CASES: [(FamilyKey, &str, &str); 6] = [
    (
        FamilyKey::S02,
        "https://site.invalid/a",
        "effective_indexing",
    ),
    (FamilyKey::S03, "https://site.invalid/", "html_occurrences"),
    (FamilyKey::S04, "https://site.invalid/", "html_occurrences"),
    (FamilyKey::S05, "https://site.invalid/a", "graph"),
    (FamilyKey::S06, "https://site.invalid/dup", "soft_404"),
    (FamilyKey::S08, "https://site.invalid/i.png", "resource"),
];

async fn run(reason: &str) {
    for (key, url, collector) in CASES {
        let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
        drop_collector(&job, url, collector, reason).await;
        let set = families::build_all(
            &job.store,
            &job.id,
            &job.report,
            CaptureMode::Wire2,
            &SystemClock,
        )
        .await;
        check_invariants(&set, &job.report);
        let s = section(&set, key);
        let n = job.report.pages.iter().position(|p| p.url == url).unwrap();
        let locator = format!("/pages/{n}");
        let for_page: Vec<_> = s
            .items
            .iter()
            .filter(|i| i.locator.as_deref() == Some(&locator))
            .collect();
        assert!(
            for_page.iter().any(|i| i.kind == "capture_omission"
                && i.outcome == Outcome::Unknown
                && i.reason.as_deref() == Some(reason)),
            "{key:?} {collector} {reason}: {for_page:?}"
        );
        assert!(
            for_page.iter().all(|i| i.outcome != Outcome::Pass),
            "{key:?}: never pass: {for_page:?}"
        );
        assert!(
            s.coverage.unknown_reasons.get(reason).copied().unwrap_or(0) >= 1,
            "{key:?}"
        );
        assert_eq!(s.state, SectionState::Partial, "{key:?}");
    }
}

#[tokio::test]
async fn collector_byte_limit_is_unknown_in_every_family() {
    run("collector_byte_limit").await;
}

#[tokio::test]
async fn run_byte_limit_is_unknown_in_every_family() {
    run("run_byte_limit").await;
}

#[tokio::test]
async fn a_family_whose_pages_all_hit_the_run_budget_is_unavailable() {
    let job = crawl(&rich_site(), 20, 10, CaptureMode::Wire2).await;
    let urls: Vec<String> =
        sqlx::query_scalar("SELECT url FROM local_page_evidence WHERE job_id=?")
            .bind(&job.id)
            .fetch_all(&job.store.0)
            .await
            .unwrap();
    for url in &urls {
        let collectors: Vec<serde_json::Value> = [
            "html_occurrences",
            "graph",
            "resource",
            "robots",
            "http_occurrences",
            "effective_indexing",
            "soft_404",
            "resource_references",
        ]
        .iter()
        .map(|c| serde_json::json!({"collector": c, "version": 1, "state": "unavailable", "reason": "run_byte_limit", "data": null}))
        .collect();
        let omission = serde_json::json!({"input_hash": "x", "capture": {"version": 1, "url": null,
            "url_omission": "run_byte_limit", "url_sha256": seosmith::model::digest(url.as_bytes()),
            "collectors": collectors}});
        sqlx::query("UPDATE local_page_evidence SET evidence=? WHERE job_id=? AND url=?")
            .bind(omission.to_string())
            .bind(&job.id)
            .bind(url)
            .execute(&job.store.0)
            .await
            .unwrap();
    }
    let set = families::build_all(
        &job.store,
        &job.id,
        &job.report,
        CaptureMode::Wire2,
        &SystemClock,
    )
    .await;
    check_invariants(&set, &job.report);
    for key in FamilyKey::ALL {
        let s = section(&set, key);
        assert_eq!(
            (s.state, s.reason.as_deref()),
            (SectionState::Unavailable, Some("run_byte_limit")),
            "{key:?}"
        );
    }
}
