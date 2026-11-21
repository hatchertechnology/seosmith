use seosmith::{
    model::{CrawlRequest, Report},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};
use uuid::Uuid;

fn cli(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .args(["--database", db.to_str().unwrap(), "operations"])
        .args(args)
        .env_remove("HTP_INGEST_URL")
        .env_remove("HTP_INGEST_TOKEN")
        .env_remove("HTP_INGEST_TOKEN_FILE")
        .env_remove("SEOSMITH_API_TOKEN")
        .env_remove("SEOSMITH_API_TOKEN_FILE")
        .output()
        .unwrap()
}
fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn request() -> CrawlRequest {
    CrawlRequest {
        request_id: Uuid::new_v4(),
        website_id: Uuid::new_v4(),
        binding_generation: 1,
        url: "http://127.0.0.1/".parse().unwrap(),
        max_pages: 1,
        max_resources: 0,
        max_duration_seconds: 10,
        delay_ms: 1000,
        check_external_links: false,
    }
}
async fn fixture(dir: &Path, state: &str) -> (Store, CrawlRequest) {
    let db = dir.join("run.sqlite");
    let store = Store::open(db.to_str().unwrap()).await.unwrap();
    sqlx::query("CREATE TABLE operations_scope(version INTEGER NOT NULL)")
        .execute(&store.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO operations_scope VALUES(1)")
        .execute(&store.0)
        .await
        .unwrap();
    std::fs::write(dir.join("run.sqlite.lock"), b"").unwrap();
    let r = request();
    store.submit(&r).await.unwrap();
    sqlx::query("UPDATE jobs SET state=?")
        .bind(state)
        .execute(&store.0)
        .await
        .unwrap();
    (store, r)
}
#[test]
fn capabilities_and_missing_binary_do_not_need_htp_or_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("missing.sqlite");
    let caps = success(&cli(&db, &["capabilities"]));
    assert_eq!(caps["adapter_version"], 1);
    assert_eq!(caps["advanced"]["ci_baseline"], false);
    assert!(!db.exists());
    assert!(
        Command::new(dir.path().join("missing-binary"))
            .arg("operations")
            .arg("capabilities")
            .output()
            .is_err()
    );
    assert!(
        !cli(&db, &["status", "--id", &Uuid::new_v4().to_string()])
            .status
            .success()
    );
    assert!(!db.exists());
}
#[test]
fn explicit_scope_validation_precedes_database_creation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let path = dir
        .path()
        .join("request $(touch SHOULD_NOT_EXIST); file.json");
    for patch in [
        json!({"max_pages":0}),
        json!({"max_resources":20001}),
        json!({"url":"https://user:secret@example.com/"}),
        json!({"max_duration_seconds":7201}),
        json!({"delay_ms":0}),
        json!({"staging_profile":"secret"}),
    ] {
        let mut value = serde_json::to_value(request()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let out = cli(&db, &["start", "--request", path.to_str().unwrap()]);
        assert!(!out.status.success());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("secret"));
        assert!(!db.exists());
    }
    let mut value = serde_json::to_value(request()).unwrap();
    value.as_object_mut().unwrap().remove("max_resources");
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(
        !cli(&db, &["start", "--request", path.to_str().unwrap()])
            .status
            .success()
    );
    std::fs::write(&path, vec![b' '; 16385]).unwrap();
    assert!(
        !cli(&db, &["start", "--request", path.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!db.exists());
}
#[test]
fn standalone_start_retry_and_changed_identity_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let path = dir
        .path()
        .join("request $(touch SHOULD_NOT_EXIST); file.json");
    let mut r = request();
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    let args = ["start", "--request", path.to_str().unwrap()];
    let first = success(&cli(&db, &args));
    // Production SSRF denial produces failed/unknown coverage without network I/O.
    assert_eq!(first["state"], "failed");
    assert_eq!(first["delivery_state"], "disabled");
    assert_eq!(first["report_available"], true);
    assert_eq!(success(&cli(&db, &args)), first);
    let report = cli(&db, &["report", "--id", &r.request_id.to_string()]);
    let report_value = success(&report);
    assert_eq!(report_value["coverage_complete"], false);
    r.max_pages = 2;
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    let conflict = cli(&db, &args);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("request_id_conflict"));
    r.request_id = Uuid::new_v4();
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    assert!(!cli(&db, &args).status.success());
}
#[tokio::test]
async fn queued_running_cancelled_and_interrupted_are_not_clean_completion() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let (store, r) = fixture(dir.path(), "queued").await;
    let id = r.request_id.to_string();
    let status = success(&cli(&db, &["status", "--id", &id]));
    assert_eq!(status["state"], "queued");
    assert_eq!(status["execution_state"], "unknown");
    store.claim().await.unwrap();
    let lock = std::fs::File::open(dir.path().join("run.sqlite.lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    let status = success(&cli(&db, &["status", "--id", &id]));
    assert_eq!(status["state"], "running");
    assert_eq!(status["worker_active"], true);
    let path = dir
        .path()
        .join("request $(touch SHOULD_NOT_EXIST); file.json");
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    assert_eq!(
        success(&cli(&db, &["start", "--request", path.to_str().unwrap()]))["state"],
        "running"
    );
    let cancelled = success(&cli(&db, &["cancel", "--id", &id]));
    assert_eq!(cancelled["cancel_accepted"], true);
    assert_eq!(cancelled["cancellation_terminal"], false);
    drop(lock);
    let cancelled = success(&cli(&db, &["cancel", "--id", &id]));
    assert_eq!(cancelled["cancel_accepted"], false);
    assert_eq!(cancelled["cancellation_terminal"], true);
    assert_eq!(cancelled["report_available"], false);
    assert!(!cli(&db, &["report", "--id", &id]).status.success());
}
#[tokio::test]
async fn exact_export_preserves_versions_unknown_fields_partial_coverage_and_no_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let (store, r) = fixture(dir.path(), "running").await;
    let report: Report = serde_json::from_value(json!({
        "schema_version": 1, "engine_version": "fixture", "ruleset_version": "fixture",
        "request": r, "started_at": "2026-11-16T00:00:00Z", "finished_at": "2026-11-16T00:00:01Z",
        "outcome": "partial", "stop_reason": "page_limit", "coverage_complete": false,
        "pages": [], "findings": []
    }))
    .unwrap();
    store.finish(&report).await.unwrap();
    let mut value = serde_json::to_value(&report).unwrap();
    value["future_evidence"] = json!({"state":"unknown"});
    let body = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
    sqlx::query("UPDATE jobs SET report=?")
        .bind(&body)
        .execute(&store.0)
        .await
        .unwrap();
    let id = r.request_id.to_string();
    let output = cli(&db, &["report", "--id", &id]);
    assert!(output.status.success());
    assert_eq!(output.stdout, body.as_bytes());
    let path = dir.path().join("export.json");
    let args = ["export", "--id", &id, "--output", path.to_str().unwrap()];
    let receipt = success(&cli(&db, &args));
    assert_eq!(receipt["sha256"], seosmith::model::digest(body.as_bytes()));
    assert_eq!(std::fs::read(&path).unwrap(), body.as_bytes());
    assert!(!cli(&db, &args).status.success());
    assert_eq!(std::fs::read(&path).unwrap(), body.as_bytes());
    assert_eq!(
        success(&cli(&db, &["cancel", "--id", &id]))["cancel_accepted"],
        false
    );
    sqlx::query("UPDATE jobs SET report=printf('%.*c', ?, 'x')")
        .bind(i64::try_from(seosmith::operations::REPORT_LIMIT + 1).unwrap())
        .execute(&store.0)
        .await
        .unwrap();
    let oversized = cli(&db, &["report", "--id", &id]);
    assert!(!oversized.status.success());
    assert_eq!(oversized.stdout, [] as [u8; 0]);
}
#[tokio::test]
async fn service_databases_are_rejected_without_recovery_or_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("service.sqlite");
    let store = Store::open(db.to_str().unwrap()).await.unwrap();
    let r = request();
    store.submit(&r).await.unwrap();
    store.claim().await.unwrap();
    let path = dir
        .path()
        .join("request $(touch SHOULD_NOT_EXIST); file.json");
    std::fs::write(&path, serde_json::to_vec(&r).unwrap()).unwrap();
    assert!(
        !cli(&db, &["start", "--request", path.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        !cli(&db, &["cancel", "--id", &r.request_id.to_string()])
            .status
            .success()
    );
    assert_eq!(
        store
            .view(&r.request_id.to_string())
            .await
            .unwrap()
            .unwrap()
            .state,
        "running"
    );
}

#[test]
fn evidence_v2_is_rejected_before_any_operation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .args([
            "--database",
            db.to_str().unwrap(),
            "--evidence-contract-v2",
            "operations",
            "capabilities",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("evidence_v2_unsupported"));
    assert!(!db.exists());
}

#[tokio::test]
async fn missing_lock_is_unknown_and_terminal_state_is_not_active() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let (store, r) = fixture(dir.path(), "running").await;
    let id = r.request_id.to_string();
    std::fs::remove_file(dir.path().join("run.sqlite.lock")).unwrap();
    let status = success(&cli(&db, &["status", "--id", &id]));
    assert!(status["worker_active"].is_null());
    assert_eq!(status["execution_state"], "unknown");
    let cancelled = success(&cli(&db, &["cancel", "--id", &id]));
    assert_eq!(cancelled["cancellation_terminal"], false);
    std::fs::write(dir.path().join("run.sqlite.lock"), b"").unwrap();
    let lock = std::fs::File::open(dir.path().join("run.sqlite.lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    sqlx::query("UPDATE jobs SET state='completed'")
        .execute(&store.0)
        .await
        .unwrap();
    let status = success(&cli(&db, &["status", "--id", &id]));
    assert_eq!(status["worker_active"], true);
    assert_eq!(status["execution_state"], "terminal");
}

#[test]
fn legacy_rules_accepts_evidence_flag_while_operations_rejects_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .args(["--evidence-contract-v2", "rules"])
        .env_remove("SEOSMITH_EVIDENCE_CONTRACT_V2")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _: Value = serde_json::from_slice(&output.stdout).unwrap();
}

#[tokio::test]
async fn legacy_crawl_and_serve_refuse_adapter_state_before_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let (store, r) = fixture(dir.path(), "running").await;
    let request_path = dir.path().join("request.json");
    let other = request();
    std::fs::write(&request_path, serde_json::to_vec(&other).unwrap()).unwrap();
    for args in [
        vec!["crawl", "--request", request_path.to_str().unwrap()],
        vec!["serve"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .args(["--database", db.to_str().unwrap()])
            .args(args)
            .env_remove("SEOSMITH_API_TOKEN")
            .env_remove("HTP_INGEST_URL")
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("operations_database_legacy_command_refused")
        );
        assert_eq!(
            store
                .view(&r.request_id.to_string())
                .await
                .unwrap()
                .unwrap()
                .state,
            "running"
        );
        assert!(
            store
                .view(&other.request_id.to_string())
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn legacy_crawl_still_operates_on_new_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("legacy.sqlite");
    let path = dir.path().join("request.json");
    std::fs::write(&path, serde_json::to_vec(&request()).unwrap()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .args([
            "--database",
            db.to_str().unwrap(),
            "crawl",
            "--request",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
}

#[test]
fn legacy_serve_still_reaches_its_normal_configuration_check() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("legacy.sqlite");
    let out = Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .args(["--database", db.to_str().unwrap(), "serve"])
        .env_remove("SEOSMITH_API_TOKEN")
        .env_remove("SEOSMITH_API_TOKEN_FILE")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("SEOSMITH_API_TOKEN"));
    assert!(db.exists());
}

#[tokio::test]
async fn empty_pre_mode_operations_database_can_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.sqlite");
    let store = Store::open(db.to_str().unwrap()).await.unwrap();
    sqlx::query("CREATE TABLE operations_scope(version INTEGER NOT NULL)")
        .execute(&store.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO operations_scope VALUES(1)")
        .execute(&store.0)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE jobs DROP COLUMN execution_mode")
        .execute(&store.0)
        .await
        .unwrap();
    drop(store);
    let path = dir.path().join("request.json");
    std::fs::write(&path, serde_json::to_vec(&request()).unwrap()).unwrap();
    let result = cli(&db, &["start", "--request", path.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
