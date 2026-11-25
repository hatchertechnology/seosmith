//! S1: production schema-4 emission, exact-byte storage, terminal HTP rejections
//! and the test-only loopback transport.
//!
//! Evidence semantics and ID equality are E-p's (`tests/schema4_evidence.rs`);
//! these tests check shape, byte identity and pointer resolution in the bytes.
use anyhow::Result;
use axum::{Router, http::StatusCode, routing::post};
use seosmith::{
    delivery::{Delivery, HTP_MAX_REPORT_BYTES},
    finding_coverage_prototype::{self, fixture_input},
    model::{Report, digest},
    policy::AdvisoryPolicy,
    store::Store,
};
use serde_json::Value;
use std::process::{Command, Stdio};

const CASES: [&str; 4] = [
    "complete-missing-h1",
    "complete-clean-h1",
    "partial-missing-h1",
    "partial-clean-h1",
];

fn wire(case: &str) -> Result<(Report, Vec<u8>)> {
    let mut report = fixture_input(case)?;
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    Ok((report, bytes))
}

#[test]
fn production_bytes_are_schema_4_with_coverage_and_evidence() -> Result<()> {
    for case in CASES {
        let (_, bytes) = wire(case)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["schema_version"], 4, "{case}");
        assert_eq!(value["policy_guidance"]["version"], "s14-wire-1", "{case}");
        assert_eq!(value["finding_coverage"]["version"], 1, "{case}");
        assert_eq!(value["evidence_contract"]["version"], 1, "{case}");
        assert!(bytes.len() <= HTP_MAX_REPORT_BYTES);
    }
    Ok(())
}

#[test]
fn fixture_form_stays_evidence_free() -> Result<()> {
    let mut report = fixture_input("complete-missing-h1")?;
    let bytes = finding_coverage_prototype::generate(&mut report, &AdvisoryPolicy::default())?;
    let value: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["schema_version"], 4);
    assert!(value.get("evidence_contract").is_none());
    Ok(())
}

#[test]
fn wire_and_prototype_serialisers_emit_the_same_findings() -> Result<()> {
    for case in CASES {
        let (_, with) = wire(case)?;
        let mut base = fixture_input(case)?;
        let without = finding_coverage_prototype::generate(&mut base, &AdvisoryPolicy::default())?;
        let (with, without): (Value, Value) = (
            serde_json::from_slice(&with)?,
            serde_json::from_slice(&without)?,
        );
        // Evidence never adds, removes or reorders findings.
        assert_eq!(with["findings"], without["findings"], "{case}");
        assert_eq!(
            with["policy_guidance"], without["policy_guidance"],
            "{case}"
        );
    }
    Ok(())
}

#[test]
fn evidence_finding_keys_resolve_in_the_serialised_bytes() -> Result<()> {
    for case in CASES {
        let (_, bytes) = wire(case)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let keys: Vec<&str> = value["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["key"].as_str().unwrap())
            .collect();
        for evaluation in value["evidence_contract"]["evaluations"]
            .as_array()
            .unwrap()
        {
            if let Some(key) = evaluation["finding_key"].as_str() {
                assert!(keys.contains(&key), "{case}: dangling evidence key {key}");
            }
        }
    }
    Ok(())
}

async fn store() -> Result<(tempfile::TempDir, Store)> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("t.sqlite").to_str().unwrap()).await?;
    Ok((dir, store))
}

async fn running_job(store: &Store, report: &mut Report) -> Result<String> {
    report.request.request_id = uuid::Uuid::new_v4();
    store.submit(&report.request).await?;
    store.claim().await?.expect("queued job");
    Ok(report.request.request_id.to_string())
}

#[tokio::test]
async fn store_keeps_the_generated_bytes_and_their_digest() -> Result<()> {
    let (_dir, store) = store().await?;
    let mut report = fixture_input("complete-missing-h1")?;
    let id = running_job(&store, &mut report).await?;
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    store.finish_bytes(&id, &report.outcome, &bytes).await?;
    let stored = store.report(&id).await?.unwrap();
    assert_eq!(stored.as_bytes(), bytes.as_slice());
    let (_, _, sha, _) = store.pending().await?.unwrap();
    assert_eq!(sha, digest(&bytes));
    // A second finalisation cannot replace the stored bytes.
    assert!(
        store
            .finish_bytes(&id, &report.outcome, &bytes)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn final_bytes_over_the_htp_cap_are_refused_by_the_store() -> Result<()> {
    // The cap is enforced on the final serialisation (after `finding_coverage`):
    // `generate_with` checks the last `to_vec`, and the store checks again.
    let (_dir, store) = store().await?;
    let mut report = fixture_input("complete-missing-h1")?;
    let id = running_job(&store, &mut report).await?;
    let bytes = vec![b' '; seosmith::delivery::HTP_MAX_REPORT_BYTES + 1];
    let error = store
        .finish_bytes(&id, &report.outcome, &bytes)
        .await
        .unwrap_err();
    assert!(error.is::<seosmith::store::ReportTooLarge>());
    Ok(())
}

#[tokio::test]
async fn finish_bytes_requires_the_evidence_contract() -> Result<()> {
    let (_dir, store) = store().await?;
    let mut report = fixture_input("complete-missing-h1")?;
    let id = running_job(&store, &mut report).await?;
    let bytes = finding_coverage_prototype::generate(&mut report, &AdvisoryPolicy::default())?;
    let value: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["schema_version"], 4);
    assert!(value.get("evidence_contract").is_none());
    assert!(
        store
            .finish_bytes(&id, &report.outcome, &bytes)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn schema_4_cannot_use_the_reserialising_finish_or_foreign_bytes() -> Result<()> {
    let (_dir, store) = store().await?;
    let mut report = fixture_input("complete-missing-h1")?;
    let id = running_job(&store, &mut report).await?;
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    let mut as_report: Report = serde_json::from_slice(&bytes)?;
    assert_eq!(as_report.schema_version, 4);
    assert!(store.finish(&as_report).await.is_err());
    // finish_bytes persists schema 4 only, and only for its own job.
    as_report.schema_version = 1;
    let legacy = serde_json::to_vec(&as_report)?;
    assert!(
        store
            .finish_bytes(&id, &report.outcome, &legacy)
            .await
            .is_err()
    );
    assert!(
        store
            .finish_bytes(&uuid::Uuid::new_v4().to_string(), &report.outcome, &bytes)
            .await
            .is_err()
    );
    assert!(store.pending().await?.is_none());
    Ok(())
}

async fn mock_htp(
    status: StatusCode,
    path: &'static str,
) -> Result<(url::Url, tokio::task::JoinHandle<()>)> {
    let app = Router::new().route(path, post(move || async move { (status, "{}") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}{path}", listener.local_addr()?).parse()?;
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Ok((endpoint, handle))
}

const NEW_ROUTE: &str = "/integrations/seosmith/policy-audits";
const LEGACY_ROUTE: &str = "/integrations/seosmith/audits";

async fn deliver_with(status: StatusCode) -> Result<(String, Option<String>, i64)> {
    deliver_to(status, NEW_ROUTE).await
}

async fn deliver_to(
    status: StatusCode,
    path: &'static str,
) -> Result<(String, Option<String>, i64)> {
    let (_dir, store) = store().await?;
    let mut report = fixture_input("complete-missing-h1")?;
    let id = running_job(&store, &mut report).await?;
    let bytes =
        finding_coverage_prototype::generate_wire_v1(&mut report, &AdvisoryPolicy::default())?;
    store.finish_bytes(&id, &report.outcome, &bytes).await?;
    let (endpoint, server) = mock_htp(status, path).await?;
    Delivery::new(endpoint, "t".repeat(32))?
        .tick(&store)
        .await?;
    server.abort();
    let view = store.view(&id).await?.unwrap();
    Ok((view.delivery_state, view.error, view.attempts))
}

#[tokio::test]
async fn htp_422_and_409_are_terminal_with_their_codes() -> Result<()> {
    assert_eq!(
        deliver_with(StatusCode::UNPROCESSABLE_ENTITY).await?,
        ("rejected".into(), Some("htp_semantic_mismatch".into()), 1)
    );
    assert_eq!(
        deliver_with(StatusCode::CONFLICT).await?,
        ("rejected".into(), Some("htp_delivery_conflict".into()), 1)
    );
    Ok(())
}

#[tokio::test]
async fn legacy_route_422_and_409_keep_retrying() -> Result<()> {
    // axum's `Json` extractor answers 422 on the legacy route: never terminal there.
    for status in [StatusCode::UNPROCESSABLE_ENTITY, StatusCode::CONFLICT] {
        assert_eq!(
            deliver_to(status, LEGACY_ROUTE).await?,
            ("pending".into(), Some("htp_delivery_failed".into()), 1),
            "{status}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn other_failures_keep_retrying_and_count_attempts() -> Result<()> {
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        assert_eq!(
            deliver_with(status).await?,
            ("pending".into(), Some("htp_delivery_failed".into()), 1),
            "{status}"
        );
    }
    Ok(())
}

fn serve(url: &str, dir: &std::path::Path, extra: &[&str]) -> std::io::Result<std::process::Child> {
    Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .env("SEOSMITH_API_TOKEN", "a".repeat(32))
        .env("HTP_INGEST_TOKEN", "b".repeat(32))
        .env("HTP_INGEST_URL", url)
        .env("SEOSMITH_POLICY_WIRE_V1", "true")
        .args(["--database", dir.join("s.sqlite").to_str().unwrap()])
        .args([
            "--advisory-policy",
            "fixtures/policies/production-default.json",
        ])
        .args(["serve", "--bind", "127.0.0.1:0"])
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
}

fn refused_with(url: &str) -> Result<Option<String>> {
    let dir = tempfile::tempdir()?;
    let mut child = serve(url, dir.path(), &[])?;
    std::thread::sleep(std::time::Duration::from_millis(1500));
    if child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
        return Ok(None);
    }
    let output = child.wait_with_output()?;
    Ok(Some(String::from_utf8_lossy(&output.stderr).into_owned()))
}

#[test]
fn https_is_required_except_for_loopback_literals_under_local_e2e() -> Result<()> {
    let loopback = refused_with("http://127.0.0.1:3000/integrations/seosmith/policy-audits")?;
    if cfg!(feature = "local-e2e") {
        assert!(loopback.is_none(), "local-e2e must accept loopback HTTP");
    } else {
        assert!(loopback.is_some_and(|e| e.contains("HTTPS required")));
    }
    for url in [
        "http://10.0.0.1/integrations/seosmith/policy-audits",
        "http://localhost:3000/integrations/seosmith/policy-audits",
        "http://127.0.0.2:3000/integrations/seosmith/policy-audits",
    ] {
        let error = refused_with(url)?;
        assert!(error.is_some_and(|e| e.contains("HTTPS required")), "{url}");
    }
    // HTTPS always starts.
    assert!(refused_with("https://127.0.0.1:9/")?.is_none());
    Ok(())
}

#[test]
fn flag_combinations_are_refused() -> Result<()> {
    let run = |envs: &[(&str, &str)], args: &[&str]| -> Result<String> {
        let dir = tempfile::tempdir()?;
        let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .env_clear()
            .env("SEOSMITH_API_TOKEN", "a".repeat(32))
            .env("HTP_INGEST_TOKEN", "b".repeat(32))
            .env("HTP_INGEST_URL", "https://127.0.0.1:9/")
            .envs(envs.iter().copied())
            .args(["--database", dir.path().join("s.sqlite").to_str().unwrap()])
            .args(args)
            .args(["serve", "--bind", "127.0.0.1:0"])
            .output()?;
        assert!(!output.status.success());
        Ok(String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let policy = [
        "--advisory-policy",
        "fixtures/policies/production-default.json",
    ];
    // The wire flag without a policy.
    assert!(
        run(&[("SEOSMITH_POLICY_WIRE_V1", "true")], &[])?
            .contains("--policy-wire-v1 requires --advisory-policy")
    );
    // The wire flag together with evidence v2.
    assert!(
        run(
            &[
                ("SEOSMITH_POLICY_WIRE_V1", "true"),
                ("SEOSMITH_EVIDENCE_CONTRACT_V2", "true")
            ],
            &policy
        )?
        .contains("carries its own evidence")
    );
    // A policy without the wire flag still cannot be served.
    assert!(run(&[], &policy)?.contains("only with --policy-wire-v1"));
    Ok(())
}

#[cfg(feature = "local-cli-fixtures")]
#[test]
fn crawl_with_the_wire_flag_prints_the_schema_4_bytes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let report: Report = serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    let mut request = report.request;
    request.request_id = uuid::Uuid::new_v4();
    request.url = "https://fixture.invalid/".parse()?;
    request.max_pages = 2;
    request.max_resources = 0;
    request.max_duration_seconds = 30;
    request.delay_ms = 1000;
    request.check_external_links = false;
    let request_path = dir.path().join("request.json");
    let fixtures = dir.path().join("responses.json");
    std::fs::write(&request_path, serde_json::to_vec(&request)?)?;
    std::fs::write(
        &fixtures,
        serde_json::to_vec(&serde_json::json!({
            "https://fixture.invalid/robots.txt": {"status":404,"body":""},
            "https://fixture.invalid/sitemap.xml": {"status":404,"body":""},
            "https://fixture.invalid/": {"status":200,"headers":{"content-type":"text/html"},"body":"<html lang=\"en\"><head><title>Home</title></head><body><p>Home</p></body></html>"}
        }))?,
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .env("SEOSMITH_LOCAL_FIXTURES", &fixtures)
        .args(["--database", dir.path().join("db.sqlite").to_str().unwrap()])
        .args([
            "--advisory-policy",
            "fixtures/policies/production-default.json",
        ])
        .arg("--policy-wire-v1")
        .args(["crawl", "--request", request_path.to_str().unwrap()])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["schema_version"], 4);
    assert!(value.get("finding_coverage").is_some());
    assert_eq!(value["evidence_contract"]["version"], 1);
    assert!(output.stdout.len() <= HTP_MAX_REPORT_BYTES);
    // Exactly the generated bytes: no pretty-printing, no trailing newline.
    assert_eq!(output.stdout, serde_json::to_vec(&value)?);
    Ok(())
}
