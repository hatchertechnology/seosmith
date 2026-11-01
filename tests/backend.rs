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
            "/robots.txt"=>Response{status:200,headers:BTreeMap::new(),body:b"User-agent: *\nDisallow: /private".to_vec(),ttfb_ms:1},
            "/sitemap.xml"=>Response{status:200,headers:BTreeMap::new(),body:b"<urlset><url><loc>https://example.com/private</loc></url><url><loc>https://example.com/orphan</loc></url></urlset>".to_vec(),ttfb_ms:1},
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
                ttfb_ms: 1,
            },
            "/sitemap.xml" | "/bad" => Response {
                status: 404,
                headers: BTreeMap::new(),
                body: Vec::new(),
                ttfb_ms: 1,
            },
            "/image.jpg" => Response {
                status: 200,
                headers: BTreeMap::from([("content-type".into(), "image/jpeg".into())]),
                body: vec![0; 150_000],
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
