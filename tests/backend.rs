use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use seosmith::{
    api::{self, App},
    audit, crawler,
    fetch::{self, Response, Transport},
    model::{CrawlRequest, Page},
    parse,
    store::Store,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;
fn request() -> CrawlRequest {
    CrawlRequest {
        request_id: Uuid::new_v4(),
        website_id: Uuid::new_v4(),
        binding_generation: 1,
        url: Url::parse("https://example.com/").unwrap(),
        max_pages: 10,
        max_duration_seconds: 30,
        delay_ms: 1000,
        check_external_links: false,
        max_resources: 500,
    }
}
fn response(html: &str) -> Response {
    Response {
        status: 200,
        headers: BTreeMap::from([("content-type".into(), "text/html".into())]),
        body: html.as_bytes().to_vec(),
        local_http_occurrences: None,
        ttfb_ms: 10,
    }
}
async fn db() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("test.sqlite").to_str().unwrap())
        .await
        .unwrap();
    (dir, store)
}
#[test]
fn denies_nonpublic_destinations_including_mapped_and_transition_addresses() {
    for ip in [
        "127.0.0.1",
        "10.42.1.2",
        "10.43.0.1",
        "169.254.169.254",
        "100.64.1.2",
        "192.168.1.2",
        "198.18.0.1",
        "::1",
        "::ffff:127.0.0.1",
        "64:ff9b::7f00:1",
        "2002:7f00:1::",
        "2001:db8::1",
        "fc00::1",
    ] {
        assert!(!fetch::public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
        assert!(fetch::public_ip(ip.parse().unwrap()), "{ip}");
    }
}
#[test]
fn parse_and_analyze_metadata_links_security_and_relations() {
    let u = Url::parse("https://example.com/").unwrap();
    let p = parse::parse(
        &u,
        0,
        true,
        &response(
            r#"<html lang="en"><head><title>Shared title for testing</title><meta name="description" content="A description long enough for a reasonable useful SEO audit test."><link rel="canonical" href="/target"><meta name="robots" content="noindex"></head><body><h1>One</h1><h3>Skipped</h3><img src="a.jpg"><a href="/missing">Broken</a><div id="a"></div><div id="a"></div><script>ignored words</script></body></html>"#,
        ),
    );
    assert_eq!(p.links[0].url, "https://example.com/missing");
    assert!(p.noindex());
    assert!(p.flags.contains(&"duplicate_ids".into()));
    let target = Page {
        url: "https://example.com/missing".into(),
        status: Some(404),
        ..Page::default()
    };
    let canonical = Page {
        url: "https://example.com/target".into(),
        status: Some(302),
        redirect: Some("https://example.com/".into()),
        ..Page::default()
    };
    let findings = audit::analyze(&[p, target, canonical], &u);
    for rule in [
        "missing_image_alt",
        "image_without_size",
        "heading_order",
        "duplicate_ids",
        "noindex",
        "sitemap_noindex",
        "sitemap_noncanonical",
        "relative_canonical",
        "broken_internal_link",
        "canonical_to_redirect",
    ] {
        assert!(findings.iter().any(|f| f.rule == rule), "{rule}");
    }
}
#[test]
fn empty_decorative_alt_is_valid_and_nonhtml_does_not_get_missing_title() {
    let u = Url::parse("https://example.com/").unwrap();
    let p = parse::parse(
        &u,
        0,
        false,
        &response(
            "<html><head><title>A useful title for testing</title></head><body><img alt=\"\" width=\"1\" height=\"1\"></body></html>",
        ),
    );
    assert!(
        !audit::analyze(&[p], &u)
            .iter()
            .any(|f| f.rule == "missing_image_alt")
    );
    let p = Page {
        url: u.to_string(),
        status: Some(404),
        ..Page::default()
    };
    assert!(
        !audit::analyze(&[p], &u)
            .iter()
            .any(|f| f.rule == "missing_title")
    );
}
#[test]
fn namespace_sitemap_and_entities_are_decoded() {
    let (index,urls)=crawler::sitemap_locations(br#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>https://example.com/?a=1&amp;b=2</loc></url></urlset>"#).unwrap();
    assert!(!index);
    assert_eq!(urls, ["https://example.com/?a=1&b=2"]);
}
#[tokio::test]
async fn persistence_idempotency_recovery_and_cancellation() {
    let (_dir, store) = db().await;
    let r = request();
    assert!(store.submit(&r).await.unwrap());
    assert!(!store.submit(&r).await.unwrap());
    let mut changed = r.clone();
    changed.max_pages = 2;
    assert!(store.submit(&changed).await.is_err());
    assert_eq!(
        store.claim().await.unwrap().unwrap().request_id,
        r.request_id
    );
    assert!(store.claim().await.unwrap().is_none());
    store.recover().await.unwrap();
    assert!(store.claim().await.unwrap().is_some());
    assert!(store.cancel(&r.request_id.to_string()).await.unwrap());
    assert!(store.cancelled(&r.request_id.to_string()).await.unwrap());
}
#[tokio::test]
async fn api_requires_auth_and_rejects_unsupported_or_unsafe_options() {
    let (_dir, store) = db().await;
    let app = api::router(App {
        store,
        token: "a".repeat(32),
    });
    assert_eq!(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/rules")
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut r = request();
    r.url = Url::parse("https://user:password@example.com/").unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/runs")
        .header("authorization", format!("Bearer {}", "a".repeat(32)))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&r).unwrap()))
        .unwrap();
    assert_eq!(
        app.oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}
struct Fixture {
    calls: Arc<Mutex<Vec<String>>>,
}
impl Transport for Fixture {
    #[allow(clippy::unused_async_trait_impl)] // Mirrors the asynchronous production transport.
    async fn get(&self, u: &Url) -> anyhow::Result<Response> {
        self.calls.lock().unwrap().push(u.to_string());
        Ok(match u.path() {
            "/robots.txt"=>Response{status:200,headers:BTreeMap::new(),body:b"User-agent: *\nDisallow: /private".to_vec(),local_http_occurrences: None,
            ttfb_ms:1},
            "/sitemap.xml"=>Response{status:200,headers:BTreeMap::new(),body:b"<urlset><url><loc>https://example.com/private</loc></url><url><loc>https://example.com/orphan</loc></url></urlset>".to_vec(),local_http_occurrences: None,
            ttfb_ms:1},
            "/"=>response("<html><head><title>Shared title for test</title></head><body><a href='/about'>About</a><a href='/private'>Private</a><a href='http://127.0.0.1/'>Never fetch</a></body></html>"),
            _=>response("<html><head><title>Shared title for test</title></head><body><h1>Page</h1></body></html>"),
        })
    }
}
#[tokio::test]
async fn full_crawl_respects_robots_and_scope_and_exports_cross_page_evidence() {
    let (_dir, store) = db().await;
    let r = request();
    store.submit(&r).await.unwrap();
    store.claim().await.unwrap();
    let fixture = Fixture {
        calls: Arc::default(),
    };
    let report = crawler::crawl_with(&store, &r, &fixture).await.unwrap();
    assert!(report.coverage_complete);
    assert_eq!(report.pages.len(), 4);
    assert!(report.findings.iter().any(|f| f.rule == "duplicate_title"));
    assert!(report.findings.iter().any(|f| f.rule == "orphan_page"));
    assert!(report.findings.iter().any(|f| f.rule == "sitemap_blocked"));
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("127.0.0.1") || u.ends_with("/private"))
    );
    store.finish(&report).await.unwrap();
    let pending = store.pending().await.unwrap().unwrap();
    store.retry(&pending.0, 0).await.unwrap();
    assert!(store.pending().await.unwrap().is_none());
    assert!(store.delivered(&pending.0, "wrong").await.is_err());
    store.delivered(&pending.0, &pending.2).await.unwrap();
    assert_eq!(
        store
            .view(&pending.0)
            .await
            .unwrap()
            .unwrap()
            .delivery_state,
        "delivered"
    );
}
#[tokio::test]
async fn page_cap_is_partial_and_checkpoint_survives_retry() {
    let (_dir, store) = db().await;
    let mut r = request();
    r.max_pages = 1;
    store.submit(&r).await.unwrap();
    store.claim().await.unwrap();
    let fixture = Fixture {
        calls: Arc::default(),
    };
    let report = crawler::crawl_with(&store, &r, &fixture).await.unwrap();
    assert_eq!(report.outcome, "partial");
    assert_eq!(report.stop_reason, "page_limit");
    assert!(!report.coverage_complete);
    assert_eq!(
        store.pages(&r.request_id.to_string()).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn push_retries_until_a_matching_receipt_then_marks_delivered() {
    use axum::{Json, Router, routing::post};
    use seosmith::{
        delivery::Delivery,
        model::{DeliveryEnvelope, DeliveryReceipt, digest},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (_dir, store) = db().await;
    let r = request();
    store.submit(&r).await.unwrap();
    store.claim().await.unwrap();
    let report = crawler::crawl_with(
        &store,
        &r,
        &Fixture {
            calls: Arc::default(),
        },
    )
    .await
    .unwrap();
    store.finish(&report).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let receiver = Router::new().route(
        "/ingest",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<DeliveryEnvelope>| {
                let count = count.clone();
                async move {
                    assert_eq!(
                        headers["authorization"],
                        format!("Bearer {}", "z".repeat(32))
                    );
                    assert_eq!(headers["idempotency-key"], body.delivery_id.to_string());
                    assert_eq!(digest(body.report_json.as_bytes()), body.sha256);
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(DeliveryReceipt {
                                delivery_id: body.delivery_id,
                                sha256: body.sha256,
                                accepted: false,
                            }),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(DeliveryReceipt {
                            delivery_id: body.delivery_id,
                            sha256: body.sha256,
                            accepted: true,
                        }),
                    )
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        Url::parse(&format!("http://{}/ingest", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, receiver).await.unwrap() });
    let delivery = Delivery::new(endpoint, "z".repeat(32)).unwrap();
    delivery.tick(&store).await.unwrap();
    assert_eq!(
        store
            .view(&r.request_id.to_string())
            .await
            .unwrap()
            .unwrap()
            .delivery_state,
        "pending"
    );
    sqlx::query("UPDATE jobs SET next_attempt=0")
        .execute(&store.0)
        .await
        .unwrap();
    delivery.tick(&store).await.unwrap();
    assert_eq!(
        store
            .view(&r.request_id.to_string())
            .await
            .unwrap()
            .unwrap()
            .delivery_state,
        "delivered"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}

#[test]
fn cycles_and_hreflang_findings_have_stable_keys() {
    let u = Url::parse("https://example.com/").unwrap();
    let a = Page {
        url: u.to_string(),
        status: Some(301),
        redirect: Some("https://example.com/b".into()),
        ..Page::default()
    };
    let b = Page {
        url: "https://example.com/b".into(),
        status: Some(302),
        redirect: Some(u.to_string()),
        ..Page::default()
    };
    let findings = audit::analyze(&[a.clone(), b.clone()], &u);
    assert_eq!(
        findings
            .iter()
            .filter(|f| f.rule == "redirect_loop")
            .count(),
        2
    );
    let repeat = audit::analyze(&[a, b], &u);
    assert_eq!(
        findings.iter().map(|f| &f.key).collect::<Vec<_>>(),
        repeat.iter().map(|f| &f.key).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn resumed_crawl_does_not_refetch_checkpointed_pages() {
    let (_dir, store) = db().await;
    let r = request();
    store.submit(&r).await.unwrap();
    store.claim().await.unwrap();
    let page = parse::parse(
        &r.url,
        0,
        false,
        &response(
            "<html><head><title>Checkpoint</title></head><body><a href='/about'>About</a></body></html>",
        ),
    );
    store
        .discover(
            &r.request_id.to_string(),
            "https://example.com/about",
            1,
            false,
            100,
        )
        .await
        .unwrap();
    store
        .save_page(&r.request_id.to_string(), &page)
        .await
        .unwrap();
    store.recover().await.unwrap();
    store.claim().await.unwrap();
    let fixture = Fixture {
        calls: Arc::default(),
    };
    let report = crawler::crawl_with(&store, &r, &fixture).await.unwrap();
    assert!(report.coverage_complete);
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|u| u == "https://example.com/")
    );
    assert!(report.pages.iter().any(|p| p.title == "Checkpoint"));
}
struct Resources;
impl Transport for Resources {
    fn get(&self, u: &Url) -> impl std::future::Future<Output = anyhow::Result<Response>> + Send {
        std::future::ready(Ok(match u.path() {
            "/robots.txt" => Response {
                status: 200,
                headers: BTreeMap::new(),
                body: Vec::new(),
                local_http_occurrences: None,
                ttfb_ms: 1,
            },
            "/sitemap.xml" | "/bad" => Response {
                status: 404,
                headers: BTreeMap::new(),
                body: Vec::new(),
                local_http_occurrences: None,
                ttfb_ms: 1,
            },
            "/image.jpg" => Response {
                status: 200,
                headers: BTreeMap::from([("content-type".into(), "image/jpeg".into())]),
                body: vec![0; 150_000],
                local_http_occurrences: None,
                ttfb_ms: 1,
            },
            _ => response(
                "<html><head><title>Resource test</title></head><body><img src='/image.jpg' alt='Test' width='10' height='10'><a href='https://other.example/bad'>External</a></body></html>",
            ),
        }))
    }
}
#[tokio::test]
async fn optional_external_observations_and_image_size_findings() {
    let (_dir, store) = db().await;
    let mut r = request();
    r.check_external_links = true;
    store.submit(&r).await.unwrap();
    store.claim().await.unwrap();
    let report = crawler::crawl_with(&store, &r, &Resources).await.unwrap();
    assert_eq!(report.pages.len(), 3);
    assert!(report.findings.iter().any(|f| f.rule == "large_image"));
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.rule == "external_link_broken")
    );
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.rule == "broken_internal_link")
    );
}

#[tokio::test]
async fn htp_size_rejection_preserves_reports_and_only_stops_oversized_413() {
    use axum::{Router, routing::post};
    use seosmith::{
        delivery::{Delivery, HTP_MAX_REPORT_BYTES},
        model::digest,
    };

    for (size, status, expected_state) in [
        (
            HTP_MAX_REPORT_BYTES,
            StatusCode::PAYLOAD_TOO_LARGE,
            "pending",
        ),
        (
            HTP_MAX_REPORT_BYTES + 1,
            StatusCode::SERVICE_UNAVAILABLE,
            "pending",
        ),
        (
            HTP_MAX_REPORT_BYTES + 1,
            StatusCode::PAYLOAD_TOO_LARGE,
            "rejected",
        ),
    ] {
        let (_dir, store) = db().await;
        let r = request();
        store.submit(&r).await.unwrap();
        // A multibyte suffix ensures the boundary is measured in bytes, not characters.
        let body = format!("{}é", "x".repeat(size - 2));
        let sha = digest(body.as_bytes());
        sqlx::query("UPDATE jobs SET state='completed',report=?,report_hash=? WHERE id=?")
            .bind(&body)
            .bind(&sha)
            .bind(r.request_id.to_string())
            .execute(&store.0)
            .await
            .unwrap();
        let receiver = Router::new().route("/", post(move || async move { status }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, receiver).await.unwrap() });
        Delivery::new(endpoint, "z".repeat(32))
            .unwrap()
            .tick(&store)
            .await
            .unwrap();
        let view = store
            .view(&r.request_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.delivery_state, expected_state);
        assert_eq!(view.attempts, 1);
        assert_eq!(
            view.error.as_deref(),
            Some(if expected_state == "rejected" {
                "htp_report_limit"
            } else {
                "htp_delivery_failed"
            })
        );
        assert_eq!(
            store
                .report(&r.request_id.to_string())
                .await
                .unwrap()
                .unwrap(),
            body
        );
        if expected_state == "rejected" {
            assert!(store.pending().await.unwrap().is_none());
            let saved_hash: String = sqlx::query_scalar("SELECT report_hash FROM jobs WHERE id=?")
                .bind(r.request_id.to_string())
                .fetch_one(&store.0)
                .await
                .unwrap();
            assert_eq!(saved_hash, sha);
        }
        server.abort();
    }
}

#[tokio::test]
async fn rejected_reports_do_not_exhaust_queue_or_expire() {
    let (_dir, store) = db().await;
    let r = request();
    store.submit(&r).await.unwrap();
    let id = r.request_id.to_string();
    let body = "immutable report";
    let sha = seosmith::model::digest(body.as_bytes());
    sqlx::query("UPDATE jobs SET state='completed', report=?, report_hash=?, created=0 WHERE id=?")
        .bind(body)
        .bind(&sha)
        .bind(&id)
        .execute(&store.0)
        .await
        .unwrap();
    store.delivery_too_large(&id, &sha).await.unwrap();
    // Fill the old queue limit with terminal rejections without changing their data.
    sqlx::query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<99) INSERT INTO jobs(id,request,state,report,report_hash,delivery_state,created) SELECT 'rejected-' || x,'{}','completed',?,?, 'rejected',0 FROM n")
        .bind(body).bind(&sha).execute(&store.0).await.unwrap();
    let next = request();
    assert!(store.submit(&next).await.unwrap());
    store.prune().await.unwrap();
    assert_eq!(store.report(&id).await.unwrap().as_deref(), Some(body));
    let retained: (String, String) =
        sqlx::query_as("SELECT report, report_hash FROM jobs WHERE id=?")
            .bind(&id)
            .fetch_one(&store.0)
            .await
            .unwrap();
    assert_eq!(retained, (body.into(), sha));
    // Pending jobs still consume the queue's existing capacity.
    sqlx::query("UPDATE jobs SET delivery_state='pending' WHERE delivery_state='rejected'")
        .execute(&store.0)
        .await
        .unwrap();
    assert!(store.submit(&request()).await.is_err());
}

#[tokio::test]
async fn local_crawl_persists_typed_evidence_across_reopen_without_delivery() {
    let (dir, store) = db().await;
    let r = request();
    let id = r.request_id.to_string();
    store.submit_local(&r).await.unwrap();
    store.claim_local(&id).await.unwrap().unwrap();
    let fixture = Fixture {
        calls: Arc::default(),
    };
    let report = crawler::crawl_local_with(&store, &r, &fixture)
        .await
        .unwrap();
    assert_eq!(report.pages.len(), 4);
    store.finish_local_only(&id).await.unwrap();
    drop(store);
    let store = Store::open(dir.path().join("test.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT url,evidence FROM local_page_evidence WHERE job_id=? ORDER BY url")
            .bind(&id)
            .fetch_all(&store.0)
            .await
            .unwrap();
    assert_eq!(rows.len(), 4);
    for (url, value) in rows {
        let value: serde_json::Value = serde_json::from_str(&value).unwrap();
        assert_eq!(value["capture"]["url"], url);
        assert_eq!(value["capture"]["collectors"].as_array().unwrap().len(), 8);
        assert_eq!(value["capture"]["collectors"][3]["collector"], "robots");
        if url.ends_with("/private") {
            assert_eq!(
                value["capture"]["collectors"][0]["reason"],
                "blocked_by_robots"
            );
        } else if url.ends_with('/') {
            assert_eq!(value["capture"]["collectors"][0]["state"], "observed");
            assert_eq!(value["capture"]["collectors"][1]["data"]["url"], url);
        }
    }
    assert!(store.pending().await.unwrap().is_none());
    assert!(store.report(&id).await.unwrap().is_none());
}

#[tokio::test]
async fn long_accepted_seed_is_checkpointed_with_bounded_url_copy() {
    let (dir, store) = db().await;
    let mut r = request();
    r.url = Url::parse(&format!("https://example.com/{}", "x".repeat(10_000))).unwrap();
    r.max_pages = 1;
    r.max_resources = 0;
    let id = r.request_id.to_string();
    store.submit_local(&r).await.unwrap();
    store.claim_local(&id).await.unwrap().unwrap();
    let fixture = Fixture {
        calls: Arc::default(),
    };
    let report = crawler::crawl_local_with(&store, &r, &fixture)
        .await
        .unwrap();
    assert_eq!(report.pages[0].url, r.url.as_str());
    store.finish_local_only(&id).await.unwrap();
    drop(store);
    let store = Store::open(dir.path().join("test.sqlite").to_str().unwrap())
        .await
        .unwrap();
    let (url, raw): (String, String) =
        sqlx::query_as("SELECT url,evidence FROM local_page_evidence WHERE job_id=?")
            .bind(&id)
            .fetch_one(&store.0)
            .await
            .unwrap();
    assert_eq!(url, r.url.as_str());
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(value["capture"]["url"].is_null());
    assert_eq!(value["capture"]["url_omission"], "url_copy_byte_limit");
    assert_eq!(
        value["capture"]["url_sha256"],
        seosmith::model::digest(url.as_bytes())
    );
    assert!(store.pending().await.unwrap().is_none());
}

struct LongDiscoveredFixture;
impl Transport for LongDiscoveredFixture {
    #[allow(clippy::unused_async_trait_impl)] // Mirrors the production transport.
    async fn get(&self, u: &Url) -> anyhow::Result<Response> {
        Ok(match u.path() {
            "/robots.txt" => Response {
                status: 200,
                headers: BTreeMap::new(),
                body: b"User-agent: *\nAllow: /".to_vec(),
                local_http_occurrences: None,
                ttfb_ms: 0,
            },
            "/sitemap.xml" => Response {
                status: 404,
                headers: BTreeMap::new(),
                body: vec![],
                local_http_occurrences: None,
                ttfb_ms: 0,
            },
            "/" => response(&format!("<a href='/{}'>Long</a>", "x".repeat(10_000))),
            _ => response("<html><body>Long target</body></html>"),
        })
    }
}
#[tokio::test]
async fn long_discovered_url_is_checkpointed() {
    let (_dir, store) = db().await;
    let mut r = request();
    r.max_pages = 2;
    r.max_resources = 0;
    let id = r.request_id.to_string();
    store.submit_local(&r).await.unwrap();
    store.claim_local(&id).await.unwrap().unwrap();
    let report = crawler::crawl_local_with(&store, &r, &LongDiscoveredFixture)
        .await
        .unwrap();
    assert_eq!(report.pages.len(), 2);
    let target = report.pages.iter().find(|p| p.url.len() > 8192).unwrap();
    let raw: String =
        sqlx::query_scalar("SELECT evidence FROM local_page_evidence WHERE job_id=? AND url=?")
            .bind(&id)
            .bind(&target.url)
            .fetch_one(&store.0)
            .await
            .unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["capture"]["url_omission"], "url_copy_byte_limit");
    assert_eq!(
        value["capture"]["url_sha256"],
        seosmith::model::digest(target.url.as_bytes())
    );
}

#[tokio::test]
async fn local_sitemap_capture_does_not_add_requests_or_change_discovery() {
    let (_local_dir, local) = db().await;
    let (_standard_dir, standard) = db().await;
    let r = request();
    let id = r.request_id.to_string();
    local.submit_local(&r).await.unwrap();
    local.claim_local(&id).await.unwrap().unwrap();
    standard.submit(&r).await.unwrap();
    standard.claim().await.unwrap().unwrap();
    let local_fixture = Fixture {
        calls: Arc::default(),
    };
    let standard_fixture = Fixture {
        calls: Arc::default(),
    };
    let local_report = crawler::crawl_local_with(&local, &r, &local_fixture)
        .await
        .unwrap();
    let standard_report = crawler::crawl_with(&standard, &r, &standard_fixture)
        .await
        .unwrap();
    assert_eq!(
        *local_fixture.calls.lock().unwrap(),
        *standard_fixture.calls.lock().unwrap()
    );
    assert_eq!(
        serde_json::to_value(local_report.pages).unwrap(),
        serde_json::to_value(standard_report.pages).unwrap()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM local_sitemap_run WHERE job_id=?")
        .bind(&id)
        .fetch_one(&standard.0)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
