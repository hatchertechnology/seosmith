use seosmith::{
    model::{Page, Report},
    s06_detector::{self, Decision, Source},
    store::Store,
};
use serde_json::{Value, json};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
};
use std::process::Command;
use uuid::Uuid;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("fixtures/s06/{name}")).unwrap()
}

fn digest(bytes: &[u8]) -> String {
    seosmith::model::digest(bytes)
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_seosmith"))
        .env_clear()
        .args(args)
        .output()
        .unwrap()
}

fn run_pair(database: &str, pair: &[&str]) -> Value {
    let mut args = vec!["--database", database, "saved-pair"];
    args.extend_from_slice(pair);
    let output = run(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn html_pair(
    database: &str,
    source: &str,
    peer: &str,
    source_status: &str,
    peer_status: &str,
) -> Value {
    run_pair(
        database,
        &[
            "--source-html",
            source,
            "--peer-html",
            peer,
            "--source-status",
            source_status,
            "--peer-status",
            peer_status,
        ],
    )
}

#[test]
fn retained_html_pair_separates_duplicates_variants_and_short_pages() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("unused.sqlite");
    let database = database.to_str().unwrap();

    let east = html_pair(
        database,
        "fixtures/s06/service-east.html",
        "fixtures/s06/service-west.html",
        "200",
        "200",
    );
    assert_eq!(east["mode"], "retained_html");
    assert_eq!(east["semantics"], s06_detector::SEMANTICS);
    assert_eq!(east["comparison"]["decision"], "candidate");
    assert_eq!(east["comparison"]["reason"], "similar main text");
    assert!(east["comparison"]["similarity"].as_f64().unwrap() >= 0.8);
    assert_eq!(
        east["peer"]["file_sha256"],
        digest(&fixture("service-west.html"))
    );
    assert_eq!(
        east["source"]["file_sha256"],
        digest(&fixture("service-east.html"))
    );
    assert_ne!(
        east["source"]["extraction"]["evidence"]["main_sha256"],
        east["peer"]["extraction"]["evidence"]["main_sha256"]
    );
    assert_eq!(east["provenance"]["network_access"], false);
    assert_eq!(east["provenance"]["raw_html_persistence"], false);

    let variants = html_pair(
        database,
        "fixtures/s06/product-red.html",
        "fixtures/s06/product-blue.html",
        "200",
        "200",
    );
    assert_eq!(variants["comparison"]["decision"], "no_candidate");
    assert_eq!(variants["comparison"]["reason"], "explicit SKU variant");

    let short = html_pair(
        database,
        "fixtures/s06/short-hours.html",
        "fixtures/s06/short-contact.html",
        "200",
        "200",
    );
    assert_eq!(short["comparison"]["decision"], "no_candidate");
    assert_eq!(
        short["comparison"]["reason"],
        "main text below similarity threshold"
    );

    let shared = html_pair(
        database,
        "fixtures/s06/service-east.html",
        "fixtures/s06/unique-service.html",
        "200",
        "200",
    );
    assert_eq!(shared["comparison"]["decision"], "no_candidate");

    let exact = html_pair(
        database,
        "fixtures/s06/service-east.html",
        "fixtures/s06/exact-copy.html",
        "200",
        "200",
    );
    assert_eq!(exact["comparison"]["decision"], "candidate");
    assert_eq!(exact["comparison"]["reason"], "identical source bytes");

    let shell = html_pair(
        database,
        "fixtures/s06/app-shell.html",
        "fixtures/s06/loading-shell.html",
        "200",
        "200",
    );
    assert_eq!(shell["comparison"]["decision"], "unknown");
    assert_eq!(shell["comparison"]["unavailable_side"], "source");

    let not_found = html_pair(
        database,
        "fixtures/s06/error-200.html",
        "fixtures/s06/error-404.html",
        "200",
        "404",
    );
    assert_eq!(not_found["comparison"]["decision"], "unknown");
    assert_eq!(not_found["comparison"]["unavailable_side"], "peer");

    let missing_status = run(&[
        "--database",
        database,
        "saved-pair",
        "--source-html",
        "fixtures/s06/service-east.html",
        "--peer-html",
        "fixtures/s06/service-west.html",
        "--source-status",
        "200",
    ]);
    assert!(!missing_status.status.success());
    assert!(String::from_utf8_lossy(&missing_status.stderr).contains("--peer-status"));

    assert!(!std::path::Path::new(database).exists());
}

fn capture(url: &str, html: &[u8]) -> anyhow::Result<String> {
    let out = s06_detector::soft_404(&Source {
        bytes: Some(html),
        status: Some(200),
        content_type: "text/html",
        complete: true,
    });
    let decision = match out.decision {
        Decision::Candidate => "candidate",
        Decision::NoCandidate => "no_candidate",
        Decision::Unknown => "unknown",
    };
    let evidence = out.evidence.as_ref().map(|evidence| {
        json!({
            "source_sha256": evidence.source_sha256,
            "main_sha256": evidence.main_sha256,
            "title": evidence.title,
            "h1": evidence.h1,
            "excerpt": evidence.excerpt,
            "template_fraction": evidence.template_fraction
        })
    });
    let state = if evidence.is_none() {
        "unavailable"
    } else if out.decision == Decision::Unknown {
        "partial"
    } else {
        "observed"
    };
    let capture = json!({
        "version": 1,
        "url": url,
        "collectors": [{
            "collector": "soft_404",
            "version": 1,
            "state": state,
            "reason": if state == "observed" { Value::Null } else { json!(out.reason) },
            "data": {
                "semantics": s06_detector::SEMANTICS,
                "decision": decision,
                "reason": out.reason,
                "evidence": evidence
            }
        }],
        "omissions": []
    });
    let input = serde_json::to_string(&capture)?;
    Ok(serde_json::to_string(&json!({
        "input_hash": digest(input.as_bytes()),
        "capture": capture
    }))?)
}

async fn quiesce(store: &Store, database: &std::path::Path) -> anyhow::Result<()> {
    store.0.close().await;
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(database)
            .journal_mode(SqliteJournalMode::Delete),
    )
    .await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn saved_checkpoint_pair_preserves_peer_identity_and_unknowns() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("pair.sqlite");
    let store = Store::open(database.to_str().unwrap()).await?;
    let mut request: Report =
        serde_json::from_str(include_str!("../fixtures/contracts/legacy.json"))?;
    request.request.request_id = Uuid::new_v4();
    request.request.url = "https://fixture.invalid/".parse()?;
    request.request.max_pages = 12;
    request.request.max_resources = 0;
    request.request.max_duration_seconds = 30;
    request.request.delay_ms = 1000;
    request.request.check_external_links = false;
    let id = request.request.request_id.to_string();
    store.submit_local(&request.request).await?;
    store.claim_local(&id).await?.unwrap();

    let pages = [
        ("https://fixture.invalid/east", "service-east.html"),
        ("https://fixture.invalid/copy", "exact-copy.html"),
        ("https://fixture.invalid/west", "service-west.html"),
        ("https://fixture.invalid/hours", "short-hours.html"),
        ("https://fixture.invalid/contact", "short-contact.html"),
        ("https://fixture.invalid/red", "product-red.html"),
        ("https://fixture.invalid/blue", "product-blue.html"),
        ("https://fixture.invalid/shell", "app-shell.html"),
        ("https://fixture.invalid/error", "error-200.html"),
    ];
    for (url, file) in pages {
        let page = Page {
            url: url.into(),
            status: Some(200),
            content_type: "text/html".into(),
            ..Page::default()
        };
        sqlx::query("INSERT INTO frontier(job_id,url,depth,in_sitemap,page) VALUES(?,?,1,0,?)")
            .bind(&id)
            .bind(url)
            .bind(serde_json::to_string(&page)?)
            .execute(&store.0)
            .await?;
        sqlx::query("INSERT INTO local_page_evidence(job_id,url,evidence) VALUES(?,?,?)")
            .bind(&id)
            .bind(url)
            .bind(capture(url, &fixture(file))?)
            .execute(&store.0)
            .await?;
    }
    // A checkpointed page without detector evidence, and a URL never discovered.
    let no_evidence = Page {
        url: "https://fixture.invalid/noevid".into(),
        status: Some(200),
        content_type: "text/html".into(),
        ..Page::default()
    };
    sqlx::query("INSERT INTO frontier(job_id,url,depth,in_sitemap,page) VALUES(?,?,1,0,?)")
        .bind(&id)
        .bind("https://fixture.invalid/noevid")
        .bind(serde_json::to_string(&no_evidence)?)
        .execute(&store.0)
        .await?;
    // A second local job stays active and cannot be compared.
    let mut active = request.request.clone();
    active.request_id = Uuid::new_v4();
    store.submit_local(&active).await?;
    store
        .claim_local(&active.request_id.to_string())
        .await?
        .unwrap();

    sqlx::query("UPDATE jobs SET state='local_only',local_finalization='completed' WHERE id=?")
        .bind(&id)
        .execute(&store.0)
        .await?;
    assert!(store.report(&id).await?.is_none());
    assert!(store.pending().await?.is_none());
    quiesce(&store, &database).await?;
    let before = std::fs::read(&database)?;
    let database = database.to_str().unwrap();

    let exact = run_pair(
        database,
        &[
            "--request-id",
            &id,
            "--source",
            "https://fixture.invalid/east",
            "--peer",
            "https://fixture.invalid/copy",
        ],
    );
    assert_eq!(exact["mode"], "saved_checkpoints");
    assert_eq!(exact["provenance"]["completion"], "completed");
    assert_eq!(
        exact["provenance"]["database_access"],
        "read_only_immutable"
    );
    assert_eq!(exact["provenance"]["network_access"], false);
    assert_eq!(exact["source"]["url"], "https://fixture.invalid/east");
    assert_eq!(exact["peer"]["url"], "https://fixture.invalid/copy");
    assert_eq!(
        exact["source"]["url_sha256"],
        digest(b"https://fixture.invalid/east")
    );
    assert_eq!(
        exact["source"]["assessment"]["evidence"]["source_sha256"],
        digest(&fixture("service-east.html"))
    );
    assert_eq!(
        exact["peer"]["assessment"]["evidence"]["source_sha256"],
        digest(&fixture("exact-copy.html"))
    );
    assert_eq!(exact["source"]["assessment"]["decision"], "no_candidate");
    assert_eq!(exact["comparison"]["decision"], "candidate");
    assert_eq!(exact["comparison"]["basis"], "identical_source_bytes");
    assert!(exact["comparison"]["similarity"].is_null());
    assert_eq!(
        exact["comparison"]["observations"]["source_bytes_equal"],
        true
    );

    let near = run_pair(
        database,
        &[
            "--request-id",
            &id,
            "--source",
            "https://fixture.invalid/east",
            "--peer",
            "https://fixture.invalid/west",
        ],
    );
    assert_eq!(near["comparison"]["decision"], "unknown");
    assert_eq!(near["comparison"]["basis"], "saved_evidence_insufficient");
    assert_eq!(
        near["comparison"]["observations"]["main_text_hash_equal"],
        false
    );

    for (source, peer) in [
        (
            "https://fixture.invalid/hours",
            "https://fixture.invalid/contact",
        ),
        (
            "https://fixture.invalid/red",
            "https://fixture.invalid/blue",
        ),
    ] {
        let result = run_pair(
            database,
            &["--request-id", &id, "--source", source, "--peer", peer],
        );
        assert_eq!(result["comparison"]["decision"], "unknown");
        assert_ne!(result["comparison"]["decision"], "candidate");
    }

    let shell = run_pair(
        database,
        &[
            "--request-id",
            &id,
            "--source",
            "https://fixture.invalid/shell",
            "--peer",
            "https://fixture.invalid/east",
        ],
    );
    assert_eq!(shell["source"]["checkpoint"]["state"], "partial");
    assert_eq!(shell["source"]["assessment"]["decision"], "unknown");
    assert_eq!(shell["comparison"]["decision"], "unknown");
    assert_eq!(shell["comparison"]["unavailable_side"], "source");
    assert_eq!(
        shell["comparison"]["reason"],
        "main content or rendered outcome unavailable"
    );

    let soft_404 = run_pair(
        database,
        &[
            "--request-id",
            &id,
            "--source",
            "https://fixture.invalid/east",
            "--peer",
            "https://fixture.invalid/error",
        ],
    );
    assert_eq!(
        soft_404["peer"]["assessment"]["decision"], "candidate",
        "the per-page soft-404 assessment remains provenance, not a pair candidate"
    );
    assert_eq!(soft_404["comparison"]["decision"], "unknown");

    let no_evidence = run_pair(
        database,
        &[
            "--request-id",
            &id,
            "--source",
            "https://fixture.invalid/east",
            "--peer",
            "https://fixture.invalid/noevid",
        ],
    );
    assert_eq!(no_evidence["comparison"]["unavailable_side"], "peer");
    assert_eq!(no_evidence["comparison"]["reason"], "page_evidence_absent");

    let absent = run_pair(
        database,
        &[
            "--request-id",
            &id,
            "--source",
            "https://fixture.invalid/east",
            "--peer",
            "https://fixture.invalid/absent",
        ],
    );
    assert_eq!(absent["comparison"]["unavailable_side"], "peer");
    assert_eq!(absent["comparison"]["reason"], "page_not_in_saved_run");

    let same = run(&[
        "--database",
        database,
        "saved-pair",
        "--request-id",
        &id,
        "--source",
        "https://fixture.invalid/east",
        "--peer",
        "https://fixture.invalid/east",
    ]);
    assert!(!same.status.success());
    assert!(String::from_utf8_lossy(&same.stderr).contains("distinct"));

    let active = run(&[
        "--database",
        database,
        "saved-pair",
        "--request-id",
        &active.request_id.to_string(),
        "--source",
        "https://fixture.invalid/",
        "--peer",
        "https://fixture.invalid/east",
    ]);
    assert!(!active.status.success());
    assert!(String::from_utf8_lossy(&active.stderr).contains("terminal local-only"));

    assert_eq!(before, std::fs::read(database)?);
    assert!(!std::path::Path::new(&format!("{database}-wal")).exists());
    Ok(())
}
