#![cfg(feature = "policy-wire-prototype")]

use serde_json::Value;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

const CASES: [&str; 4] = [
    "complete-missing-h1",
    "complete-clean-h1",
    "partial-missing-h1",
    "partial-clean-h1",
];

fn fixture(case: &str) -> Vec<u8> {
    std::fs::read(format!(
        "fixtures/contracts/s14-finding-coverage-v1/{case}.json"
    ))
    .unwrap()
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}

async fn save_report(path: &Path, id: Uuid, report: &[u8]) -> anyhow::Result<()> {
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete),
    )
    .await?;
    sqlx::query("CREATE TABLE jobs (id TEXT PRIMARY KEY, report TEXT)")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO jobs (id, report) VALUES (?, ?)")
        .bind(id.to_string())
        .bind(std::str::from_utf8(report)?)
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

async fn stored_report(path: &Path, id: Uuid) -> anyhow::Result<String> {
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .immutable(true),
    )
    .await?;
    let report = sqlx::query_scalar("SELECT report FROM jobs WHERE id=?")
        .bind(id.to_string())
        .fetch_one(&pool)
        .await?;
    pool.close().await;
    Ok(report)
}

fn run_export(database: &Path, id: Uuid) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .args([
            "--database",
            database.to_str().unwrap(),
            "saved-export",
            &id.to_string(),
        ])
        .output()
        .unwrap()
}

#[tokio::test]
async fn committed_cases_are_reproduced_from_saved_reports() -> anyhow::Result<()> {
    for case in CASES {
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("saved.sqlite");
        let id = Uuid::new_v4();
        let raw = fixture(case);
        save_report(&database, id, &raw).await?;

        let output = run_export(&database, id);
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, raw, "{case}: emitted bytes");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("sha256="),
            "{case}: digest summary"
        );
        assert_eq!(
            stored_report(&database, id).await?.as_bytes(),
            raw.as_slice(),
            "{case}: raw report changed"
        );
        assert!(!sidecar(&database, "-wal").exists(), "{case}: WAL created");
        assert!(
            !sidecar(&database, "-journal").exists(),
            "{case}: journal created"
        );
    }
    Ok(())
}

#[tokio::test]
async fn schema_one_report_uses_the_default_policy() -> anyhow::Result<()> {
    let mut value: Value = serde_json::from_slice(&fixture("complete-missing-h1"))?;
    value["schema_version"] = 1.into();
    value.as_object_mut().unwrap().remove("policy_guidance");
    value.as_object_mut().unwrap().remove("finding_coverage");
    let raw = serde_json::to_vec(&value)?;

    let dir = tempfile::tempdir()?;
    let database = dir.path().join("saved.sqlite");
    let id = Uuid::new_v4();
    save_report(&database, id, &raw).await?;

    let output = run_export(&database, id);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, fixture("complete-missing-h1"));
    assert_eq!(
        stored_report(&database, id).await?.as_bytes(),
        raw.as_slice()
    );
    Ok(())
}

#[tokio::test]
async fn evidence_contract_reports_are_refused_without_writes() -> anyhow::Result<()> {
    let raw = std::fs::read("fixtures/contracts/v2-complete.json")?;
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("saved.sqlite");
    let id = Uuid::new_v4();
    save_report(&database, id, &raw).await?;

    let output = run_export(&database, id);
    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("evidence-contract"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stored_report(&database, id).await?.as_bytes(),
        raw.as_slice()
    );
    assert!(!sidecar(&database, "-wal").exists());
    assert!(!sidecar(&database, "-journal").exists());
    Ok(())
}

#[test]
fn missing_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("absent.sqlite");
    let output = run_export(&database, Uuid::new_v4());
    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("database does not exist"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
