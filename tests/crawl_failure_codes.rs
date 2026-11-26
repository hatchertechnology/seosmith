//! Named job-failure codes (WP8b-1, design `wave1-wp8a.md` §4, 64-1).
//!
//! Part 1 reaches the generation codes through minimal mutations of valid
//! reports. Part 2 drives the real `serve` worker (feature-gated, offline,
//! through `FixtureTransport`) and checks that `jobs.error` carries the code,
//! that the code is logged without request data or tokens, and that the worker
//! keeps running and finalizes the next job.
use anyhow::Result;
use seosmith::{
    finding_coverage_prototype::{self, fixture_input},
    generation_error::JobErrorCode,
    policy::AdvisoryPolicy,
};

#[path = "../examples/make_scale_fixture.rs"]
mod scale;

fn code_of(report: &mut seosmith::model::Report) -> JobErrorCode {
    finding_coverage_prototype::generate_wire_v1(report, &AdvisoryPolicy::default())
        .expect_err("the mutated report must be rejected")
        .code
}

#[test]
fn byte_cap_is_report_byte_limit() -> Result<()> {
    // Many pages with strings just under the per-string limit: the final bytes
    // exceed the HTP cap without tripping the string limit.
    let mut report = fixture_input("complete-missing-h1")?;
    let template = report.pages[0].clone();
    report.pages = (0..230)
        .map(|i| {
            let mut page = template.clone();
            page.url = format!("https://example.com/p{i}");
            page.title = format!("{i:05}{}", "t".repeat(12_000));
            page.description = format!("{i:05}{}", "d".repeat(12_000));
            page
        })
        .collect();
    assert_eq!(code_of(&mut report), JobErrorCode::ReportByteLimit);
    Ok(())
}

#[test]
fn manifest_cap_is_policy_manifest_limit() -> Result<()> {
    let mut report = scale::scale_report(scale::Shape::site(1_696, 8))?;
    assert_eq!(code_of(&mut report), JobErrorCode::PolicyManifestLimit);
    Ok(())
}

#[test]
fn a_duplicate_source_entity_is_generation_invariant() -> Result<()> {
    let mut report = fixture_input("complete-missing-h1")?;
    let again = report.pages[0].clone();
    report.pages.push(again);
    assert_eq!(code_of(&mut report), JobErrorCode::GenerationInvariant);
    Ok(())
}

#[test]
fn an_unclassified_internal_error_is_never_reported_as_a_size_limit() -> Result<()> {
    // A request that fails validation inside generation is untyped at the source.
    let mut report = fixture_input("complete-missing-h1")?;
    report.request.max_pages = 0;
    let code = code_of(&mut report);
    assert_eq!(code, JobErrorCode::GenerationInvariant);
    Ok(())
}

#[cfg(all(feature = "local-cli-fixtures", feature = "local-e2e"))]
mod serve {
    use super::scale;
    use anyhow::Result;
    use seosmith::{model::CrawlRequest, store::Store};
    use sqlx::sqlite::SqliteConnectOptions;
    use std::fmt::Write as _;
    use std::{
        collections::BTreeMap,
        path::Path,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    const API_TOKEN: &str = "test-api-token-0123456789abcdef0123456789";
    const INGEST_TOKEN: &str = "test-ingest-token-0123456789abcdef012345";

    struct Worker(Child);
    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn free_port() -> Result<u16> {
        Ok(std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port())
    }

    fn entry(status: u16, content_type: &str, body: &str) -> serde_json::Value {
        serde_json::json!({"status": status, "headers": {"content-type": content_type}, "body": body})
    }

    /// Two hosts: `scale.example` (57 pages, `resources` assets) and
    /// `small.example` (one page). Returns the fixture map and the URL count.
    fn fixtures(resources: usize) -> BTreeMap<String, serde_json::Value> {
        let mut map = BTreeMap::new();
        let pages = scale::SCALE_PAGES;
        for host in [scale::SCALE_HOST, "https://small.example"] {
            map.insert(format!("{host}/robots.txt"), entry(404, "text/plain", ""));
            map.insert(format!("{host}/sitemap.xml"), entry(404, "text/plain", ""));
        }
        map.insert(
            "https://small.example/".into(),
            entry(
                200,
                "text/html",
                "<html lang=\"en\"><head><title>Small site home page</title></head><body><p>Home</p></body></html>",
            ),
        );
        for i in 0..pages {
            let mut html = format!(
                "<html lang=\"en\"><head><title>Scale fixture page number {i} of the site</title>"
            );
            for k in (i..resources).step_by(pages) {
                if k % 2 == 0 {
                    write!(html, "<link rel=\"stylesheet\" href=\"/assets/r{k}.css\">").unwrap();
                } else {
                    write!(html, "<script src=\"/assets/r{k}.js\"></script>").unwrap();
                }
            }
            html.push_str("</head><body>");
            for step in 1..=3 {
                write!(html, "<a href=\"/p{}\">n</a>", (i + step) % pages).unwrap();
            }
            for j in 0..8 {
                write!(html, "<a href=\"https://external.example/{i}/{j}\">e</a>").unwrap();
            }
            html.push_str("</body></html>");
            let path = if i == 0 {
                String::new()
            } else {
                format!("p{i}")
            };
            map.insert(
                format!("{}/{path}", scale::SCALE_HOST),
                entry(200, "text/html", &html),
            );
        }
        for k in 0..resources {
            let (ext, content_type) = if k % 2 == 0 {
                ("css", "text/css")
            } else {
                ("js", "text/javascript")
            };
            map.insert(
                format!("{}/assets/r{k}.{ext}", scale::SCALE_HOST),
                entry(200, content_type, "x{}"),
            );
        }
        map
    }

    fn request(
        url: &str,
        id: u128,
        max_pages: usize,
        max_resources: usize,
    ) -> Result<CrawlRequest> {
        Ok(serde_json::from_value(serde_json::json!({
            "request_id": uuid::Uuid::from_u128(id),
            "website_id": uuid::Uuid::from_u128(id + 1),
            "binding_generation": 1,
            "url": url,
            "max_pages": max_pages,
            "max_duration_seconds": 3600,
            "delay_ms": 1000,
            "check_external_links": false,
            "max_resources": max_resources
        }))?)
    }

    /// Submit through the public path (validated), then let a test patch the
    /// stored request. The worker's fixture seam skips the pacing delay itself.
    async fn submit(
        store: &Store,
        r: &CrawlRequest,
        patch: impl Fn(&mut serde_json::Value),
    ) -> Result<()> {
        store.submit(r).await?;
        let mut value = serde_json::to_value(r)?;
        patch(&mut value);
        sqlx::query("UPDATE jobs SET request=? WHERE id=?")
            .bind(serde_json::to_string(&value)?)
            .bind(r.request_id.to_string())
            .execute(&store.0)
            .await?;
        Ok(())
    }

    struct Outcome {
        jobs: BTreeMap<String, (String, Option<String>)>,
        stderr: String,
    }

    async fn run_worker(dir: &Path, ids: &[String]) -> Result<Outcome> {
        let db = dir.join("db.sqlite");
        let stderr_path = dir.join("serve.stderr");
        let port = free_port()?;
        let child = Command::new(env!("CARGO_BIN_EXE_seosmith"))
            .env_clear()
            .env("SEOSMITH_LOCAL_FIXTURES", dir.join("responses.json"))
            .env("SEOSMITH_API_TOKEN", API_TOKEN)
            .env("HTP_INGEST_URL", "http://127.0.0.1:1/ingest")
            .env("HTP_INGEST_TOKEN", INGEST_TOKEN)
            .args(["--database", db.to_str().unwrap()])
            .args([
                "--advisory-policy",
                "fixtures/policies/production-default.json",
            ])
            .arg("--policy-wire-v1")
            .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
            .stderr(std::fs::File::create(&stderr_path)?)
            .stdout(Stdio::null())
            .spawn()?;
        let _worker = Worker(child);
        let pool = loop {
            // The worker owns the file lock, not the SQLite file: read-only
            // access is enough to observe it.
            if let Ok(pool) = sqlx::SqlitePool::connect_with(
                SqliteConnectOptions::new().filename(&db).read_only(true),
            )
            .await
            {
                break pool;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let started = Instant::now();
        loop {
            let rows: Vec<(String, String, Option<String>)> =
                sqlx::query_as("SELECT id,state,error FROM jobs ORDER BY created,id")
                    .fetch_all(&pool)
                    .await?;
            let settled = ids.iter().all(|id| {
                rows.iter()
                    .any(|(i, s, _)| i == id && !matches!(s.as_str(), "queued" | "running"))
            });
            if settled {
                return Ok(Outcome {
                    jobs: rows.into_iter().map(|(i, s, e)| (i, (s, e))).collect(),
                    stderr: std::fs::read_to_string(&stderr_path)?,
                });
            }
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(240),
                "worker did not settle: {rows:?}\n{}",
                std::fs::read_to_string(&stderr_path).unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn setup(dir: &Path, resources: usize) -> Result<Store> {
        std::fs::write(
            dir.join("responses.json"),
            serde_json::to_vec(&fixtures(resources))?,
        )?;
        Store::open(dir.join("db.sqlite").to_str().unwrap()).await
    }

    #[tokio::test]
    async fn serve_names_policy_manifest_limit_over_1753_urls_and_finalizes_the_next_job()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = setup(dir.path(), 1_696).await?;
        let big = request("https://scale.example/", 0x100, 100, 3_000)?;
        let small = request("https://small.example/", 0x200, 5, 0)?;
        submit(&store, &big, |_| {}).await?;
        submit(&store, &small, |_| {}).await?;
        drop(store);
        let ids = [big.request_id.to_string(), small.request_id.to_string()];
        let out = run_worker(dir.path(), &ids).await?;
        let (state, error) = &out.jobs[&ids[0]];
        assert_eq!(
            (state.as_str(), error.as_deref()),
            ("failed", Some("policy_manifest_limit")),
            "{}",
            out.stderr
        );
        // The worker survived and finalized the next job.
        // The ingest URL points at a closed port, so the delivery loop may already have
        // recorded `htp_delivery_failed` on the completed job by the time we look: the
        // job state is what shows the worker survived.
        let (state, error) = &out.jobs[&ids[1]];
        assert!(
            state == "completed" && matches!(error.as_deref(), None | Some("htp_delivery_failed")),
            "{state} {error:?}\n{}",
            out.stderr
        );
        // The code is logged, with no request data, secrets or error chains.
        assert!(
            out.stderr.contains("policy_manifest_limit"),
            "{}",
            out.stderr
        );
        for secret in [API_TOKEN, INGEST_TOKEN, "scale.example", "Caused by"] {
            assert!(
                !out.stderr.contains(secret),
                "{secret} leaked:\n{}",
                out.stderr
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn serve_keeps_the_fallback_for_a_crawl_error_and_continues() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = setup(dir.path(), 0).await?;
        let broken = request("https://small.example/", 0x300, 5, 0)?;
        let small = request("https://small.example/", 0x400, 5, 0)?;
        // A duration that cannot become an i64 makes the crawl future return Err.
        submit(&store, &broken, |v| {
            v["max_duration_seconds"] = u64::MAX.into();
        })
        .await?;
        submit(&store, &small, |_| {}).await?;
        drop(store);
        let ids = [broken.request_id.to_string(), small.request_id.to_string()];
        let out = run_worker(dir.path(), &ids).await?;
        let (state, error) = &out.jobs[&ids[0]];
        assert_eq!(
            (state.as_str(), error.as_deref()),
            ("failed", Some("crawl_internal_error")),
            "{}",
            out.stderr
        );
        let (state, _) = &out.jobs[&ids[1]];
        assert_eq!(state, "completed", "{}", out.stderr);
        assert!(
            out.stderr.contains("crawl_internal_error"),
            "{}",
            out.stderr
        );
        // The previously unlogged cause is now visible, top-level only.
        assert!(out.stderr.contains("crawl failed"), "{}", out.stderr);
        Ok(())
    }
}
