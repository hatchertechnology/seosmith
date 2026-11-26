//! WP8b-3 with the real 8b-2 builders: a captured crawl, then `build_all`, then
//! the wire-2 report, with no hand-made family sections.
use anyhow::Result;
use seosmith::{
    crawler,
    families::{
        build_all,
        types::{CaptureMode, FamilyKey, SystemClock},
    },
    policy::AdvisoryPolicy,
    store::Store,
    wire2::generate_wire_v2,
};
use serde_json::Value;

#[path = "support/site.rs"]
mod site;
use site::{Site, request, unpaced};

fn small_site() -> Site {
    Site::default()
        .robots("User-agent: *\nAllow: /\n")
        .html(
            "https://site.invalid/",
            "<html lang='en'><head><title>Home page of the small site</title></head><body><h1>Home</h1><a href='/a'>a</a><a href='/b'>b</a><a href='https://other.invalid/x'>x</a></body></html>",
        )
        .html("https://site.invalid/a", "<html lang='en'><head><title>A page about the first thing</title></head><body><h1>A</h1></body></html>")
        .html("https://site.invalid/b", "<html lang='en'><head><title></title></head><body><a href='/a'>a</a></body></html>")
}

async fn report_for(
    claim: CaptureMode,
    crawl: CaptureMode,
) -> Result<(Store, tempfile::TempDir, seosmith::model::Report, String)> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("s.sqlite").to_str().unwrap()).await?;
    let r = request(10, 0);
    store.submit(&r).await?;
    store.claim_with(claim).await?.unwrap();
    let mut report =
        crawler::crawl_captured_with(&store, &unpaced(&r), crawl, &small_site()).await?;
    // As `main::crawl_job` does: the report keeps the real politeness delay.
    report.request.delay_ms = r.delay_ms;
    let id = r.request_id.to_string();
    Ok((store, dir, report, id))
}

/// With `WIRE2_E2E_DUMP_DIR` set, keeps the produced bytes for the receiver's
/// `admit` example (a manual check; the tests do not depend on it).
fn dump(name: &str, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = std::env::var_os("WIRE2_E2E_DUMP_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            bytes,
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn a_captured_crawl_yields_a_wire2_report_with_real_families() -> Result<()> {
    let (store, _dir, mut report, id) = report_for(CaptureMode::Wire2, CaptureMode::Wire2).await?;
    let stored = store.capture_mode(&id).await?;
    assert_eq!(stored, CaptureMode::Wire2);
    let families = build_all(&store, &id, &report, stored, &SystemClock).await;
    let bytes = generate_wire_v2(&mut report, &AdvisoryPolicy::default(), families)?;
    dump("captured", &bytes)?;
    let v: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(v["policy_guidance"]["version"], "s14-wire-2");
    let manifest = serde_json::to_vec(&v["policy_guidance"]["manifest"])?.len();
    assert!(manifest <= 16_384, "{manifest}");
    let sections = v["evidence_families"]["families"].as_object().unwrap();
    assert_eq!(sections.len(), 6);
    let mut produced = 0;
    for key in FamilyKey::ALL {
        let s = &sections[key.as_str()];
        // The wire never degraded a real builder's section.
        assert_ne!(s["reason"], "analyzer_error", "{}: {s}", key.as_str());
        assert_ne!(s["reason"], "capture_mode_changed", "{}", key.as_str());
        if s["state"] != "unavailable" {
            produced += 1;
        }
        let kept = s["items"].as_array().unwrap().len() as u64;
        assert_eq!(
            s["items_total"].as_u64().unwrap(),
            kept + s["omitted_items"].as_u64().unwrap()
        );
    }
    assert!(produced >= 4, "only {produced} families produced: {v}");
    assert!(bytes.len() <= seosmith::delivery::HTP_MAX_REPORT_BYTES);
    Ok(())
}

#[tokio::test]
async fn a_job_stored_as_none_gets_capture_mode_changed_in_every_family() -> Result<()> {
    let (store, _dir, mut report, id) = report_for(CaptureMode::None, CaptureMode::None).await?;
    let stored = store.capture_mode(&id).await?;
    assert_eq!(stored, CaptureMode::None);
    let families = build_all(&store, &id, &report, stored, &SystemClock).await;
    let bytes = generate_wire_v2(&mut report, &AdvisoryPolicy::default(), families)?;
    dump("none", &bytes)?;
    let v: Value = serde_json::from_slice(&bytes)?;
    for (_, s) in v["evidence_families"]["families"].as_object().unwrap() {
        assert_eq!(s["state"], "unavailable");
        assert_eq!(s["reason"], "capture_mode_changed");
    }
    Ok(())
}
