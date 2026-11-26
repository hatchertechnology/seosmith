//! seosmith#64 offline reproduction (WP8b-1, design `wave1-wp8a.md` §4, 64-2).
//!
//! The geometry is the failing run: 57 in-scope pages plus 1,696 in-scope
//! resources, about 8 out-of-scope links per page. Everything here is
//! deterministic and offline. The numbers printed with `--nocapture` are the
//! measurements the closing comment of #64 cites.
use anyhow::Result;
use seosmith::{
    delivery::HTP_MAX_REPORT_BYTES,
    finding_coverage_prototype,
    generation_error::JobErrorCode,
    policy::AdvisoryPolicy,
    policy_candidate::{ManifestRow, generate_unvalidated},
};
use std::time::Instant;

#[path = "../examples/make_scale_fixture.rs"]
mod scale;
use scale::{Shape, scale_report};

const V1_MANIFEST_CAP: usize = 262_144;

fn manifest_bytes(shape: Shape) -> Result<usize> {
    let report = scale_report(shape)?;
    let (_, manifest): (_, Vec<ManifestRow>) =
        generate_unvalidated(&report, &AdvisoryPolicy::default())?;
    Ok(serde_json::to_vec(&manifest)?.len())
}

/// Serialized `policy_guidance` of a delivering report.
fn guidance_bytes(shape: Shape) -> Result<usize> {
    let mut report = scale_report(shape)?;
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(serde_json::to_vec(&value["policy_guidance"])?.len())
}

#[test]
fn a_and_b_the_failing_geometry_is_named_policy_manifest_limit() -> Result<()> {
    let shape = Shape::site(1_696, 8);
    // (a) the in-memory manifest is over the v1 byte limit.
    let manifest = manifest_bytes(shape)?;
    println!(
        "64-2 (a): 57 pages + 1696 resources, 8 external links/page: manifest = {manifest} B (limit {V1_MANIFEST_CAP})"
    );
    assert!(manifest > V1_MANIFEST_CAP, "{manifest}");
    // (b) generation fails with the named code, not the untyped fallback.
    let mut report = scale_report(shape)?;
    let error =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())
            .expect_err("v1 cannot carry this manifest");
    println!(
        "64-2 (b): code = {}, detail = {}",
        error.code.as_str(),
        error.detail
    );
    assert_eq!(error.code, JobErrorCode::PolicyManifestLimit);
    assert_eq!(error.code.as_str(), "policy_manifest_limit");
    assert!(
        error.detail.contains("manifest byte limit"),
        "{}",
        error.detail
    );
    Ok(())
}

#[test]
fn c_the_same_site_with_250_resources_delivers() -> Result<()> {
    let mut report = scale_report(Shape::site(250, 8))?;
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    let manifest = manifest_bytes(Shape::site(250, 8))?;
    println!(
        "64-2 (c): 57 + 250: manifest = {manifest} B, report = {} B",
        bytes.len()
    );
    assert!(manifest < V1_MANIFEST_CAP);
    assert!(bytes.len() <= HTP_MAX_REPORT_BYTES);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?["schema_version"],
        4
    );
    Ok(())
}

fn within(measured: f64, expected: f64, tolerance: f64) -> bool {
    (measured - expected).abs() <= expected * tolerance
}

#[test]
fn d_manifest_slopes_match_the_code_model() -> Result<()> {
    // Per in-scope resource: 8 ids x 74 B = 592 B (F13).
    let (r_lo, r_hi) = (100, 200);
    let lo = manifest_bytes(Shape::site(r_lo, 8))?;
    let hi = manifest_bytes(Shape::site(r_hi, 8))?;
    #[allow(clippy::cast_precision_loss)]
    let per_resource = (hi - lo) as f64 / (r_hi - r_lo) as f64;
    // Per out-of-scope edge id: about 82 B (F13).
    let (e_lo, e_hi) = (4, 12);
    let few = manifest_bytes(Shape::site(100, e_lo))?;
    let many = manifest_bytes(Shape::site(100, e_hi))?;
    #[allow(clippy::cast_precision_loss)]
    let per_edge = (many - few) as f64 / (scale::SCALE_PAGES * (e_hi - e_lo)) as f64;
    // The model M = 48,035 + 592 R + 82 E at 57 pages, for the record.
    let model = 48_035.0 + 592.0 * 100.0 + 82.0 * (57.0 * 8.0);
    let at_100 = manifest_bytes(Shape::site(100, 8))?;
    // policy_guidance slope (F10 attributes 648 B/resource to it).
    let g_lo = guidance_bytes(Shape::site(r_lo, 8))?;
    let g_hi = guidance_bytes(Shape::site(r_hi, 8))?;
    #[allow(clippy::cast_precision_loss)]
    let guidance_slope = (g_hi - g_lo) as f64 / (r_hi - r_lo) as f64;
    println!("64-2 (d): manifest per in-scope resource = {per_resource:.1} B (model 592)");
    println!("64-2 (d): manifest per edge id           = {per_edge:.1} B (model ~82)");
    println!("64-2 (d): model M(100,456) = {model:.0} B, measured = {at_100} B");
    println!(
        "64-2 (d): policy_guidance per resource   = {guidance_slope:.1} B (F10 attributes 648)"
    );
    assert!(within(per_resource, 592.0, 0.10), "{per_resource}");
    assert!(within(per_edge, 82.0, 0.10), "{per_edge}");
    Ok(())
}

/// Wall time of the whole v1 pipeline (which runs `generate` three times) at
/// the largest geometry v1 can still deliver. The hash-heavy universe check is
/// what F15 replaces; compare the printed times before and after.
#[test]
fn generation_wall_time_is_recorded() -> Result<()> {
    let started = Instant::now();
    let mut report = scale_report(Shape::site(290, 8))?;
    let build = started.elapsed();
    let started = Instant::now();
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    println!(
        "64-1 timing: 57 + 290 resources: build {build:?}, generate_wire_v1 {:?}, {} B",
        started.elapsed(),
        bytes.len()
    );
    Ok(())
}
