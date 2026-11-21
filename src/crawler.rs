use crate::{
    audit,
    fetch::{Fetcher, MAX_BODY, Transport},
    model::{self, CrawlRequest, Page, Report, normalize},
    parse,
    store::Store,
};
use anyhow::{Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    time::{Duration, Instant},
};
use url::Url;
struct Robots {
    body: String,
    source: &'static str,
}
impl Robots {
    fn allowed(&self, url: &Url) -> bool {
        robotstxt::DefaultMatcher::default().one_agent_allowed_by_robots(
            &self.body,
            "Seosmith",
            url.as_str(),
        )
    }
}
fn origin(u: &Url) -> String {
    u.origin().ascii_serialization()
}
async fn robots(fetch: &impl Transport, u: &Url) -> Result<Robots> {
    let r = fetch.get(&u.join("/robots.txt")?).await?;
    match r.status {
        200 => {
            let source = if std::str::from_utf8(&r.body).is_ok() {
                "robots_txt"
            } else {
                "robots_encoding_lossy"
            };
            Ok(Robots {
                body: String::from_utf8_lossy(&r.body).into_owned(),
                source,
            })
        }
        404 | 410 => Ok(Robots {
            body: String::new(),
            source: "not_found",
        }),
        401 | 403 => Ok(Robots {
            body: "User-agent: *\nDisallow: /".into(),
            source: "http_denied_synthetic",
        }),
        _ => bail!("robots_unavailable"),
    }
}
pub fn sitemap_locations(body: &[u8]) -> Result<(bool, Vec<String>)> {
    let decoded;
    let bytes = if body.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(body)
            .take((MAX_BODY + 1) as u64)
            .read_to_end(&mut out)?;
        ensure!(out.len() <= MAX_BODY, "expanded sitemap too large");
        decoded = out;
        decoded.as_slice()
    } else {
        body
    };
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut is_index = false;
    let mut in_loc = false;
    let mut loc = String::new();
    let mut urls = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                if e.local_name().as_ref() == "sitemapindex" {
                    is_index = true;
                }
                if e.local_name().as_ref() == "loc" {
                    in_loc = true;
                    loc.clear();
                }
            }
            Event::Text(e) if in_loc => loc.push_str(&quick_xml::escape::unescape(e.as_ref())?),
            Event::CData(e) if in_loc => loc.push_str(e.as_ref()),
            Event::GeneralRef(e) if in_loc => {
                let entity = format!("&{};", e.as_ref());
                loc.push_str(&quick_xml::escape::unescape(&entity)?);
            }
            Event::End(e) if e.local_name().as_ref() == "loc" => {
                in_loc = false;
                if !loc.is_empty() {
                    urls.push(loc.clone());
                }
                ensure!(urls.len() <= 100_000, "too many sitemap entries");
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((is_index, urls))
}
async fn discover_sitemaps(
    store: &Store,
    r: &CrawlRequest,
    fetch: &impl Transport,
    body: &str,
    collect: bool,
) -> Result<bool> {
    let id = r.request_id.to_string();
    let mut maps: Vec<Url> = body
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("sitemap")
                .then(|| normalize(&r.url, v))
                .flatten()
        })
        .filter(|u| r.in_scope(u))
        .collect();
    if maps.is_empty() {
        maps.push(r.url.join("/sitemap.xml")?);
    }
    let mut seen = HashSet::new();
    let mut complete = true;
    while let Some(u) = maps.pop() {
        if !(Robots {
            body: body.into(),
            source: "robots_txt",
        })
        .allowed(&u)
        {
            if collect {
                store
                    .save_s02_s03_sitemap(
                        &id,
                        u.as_str(),
                        &serde_json::json!({"version":"s02-s03-local-1", "state":"robots_blocked"}),
                    )
                    .await?;
            }
            complete = false;
            continue;
        }
        if !seen.insert(u.to_string()) {
            continue;
        }
        if seen.len() > 20 {
            if collect {
                store.save_s02_s03_sitemap(&id, u.as_str(), &serde_json::json!({"version":"s02-s03-local-1", "state":"uncollected", "reason":"sitemap_limit"})).await?;
                for pending in &maps {
                    store.save_s02_s03_sitemap(&id, pending.as_str(), &serde_json::json!({"version":"s02-s03-local-1", "state":"uncollected", "reason":"sitemap_limit"})).await?;
                }
            }
            complete = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(r.delay_ms)).await;
        let fetched = fetch.get(&u).await;
        if collect {
            let evidence = match &fetched {
                Ok(resp) if resp.status == 200 => {
                    serde_json::json!({"version":"s02-s03-local-1", "state":"available", "status":resp.status, "document":crate::sitemap_evidence::parse(u.as_str(), &resp.body, MAX_BODY, 2_000)})
                }
                Ok(resp) if resp.status == 200 => {
                    serde_json::json!({"version":"s02-s03-local-1", "state":"unavailable", "status":resp.status, "reason":"compressed_capture_unavailable"})
                }
                Ok(resp) => {
                    serde_json::json!({"version":"s02-s03-local-1", "state":"http_error", "status":resp.status})
                }
                Err(_) => serde_json::json!({"version":"s02-s03-local-1", "state":"fetch_failed"}),
            };
            store
                .save_s02_s03_sitemap(&id, u.as_str(), &evidence)
                .await?;
        }
        match fetched {
            Ok(resp) if resp.status == 200 => match sitemap_locations(&resp.body) {
                Ok((index, urls)) => {
                    for url in urls {
                        if let Some(target) = normalize(&u, &url).filter(|u| r.in_scope(u)) {
                            if index {
                                maps.push(target);
                            } else {
                                store
                                    .discover(&id, target.as_str(), 1, true, r.max_pages * 10)
                                    .await?;
                            }
                        }
                    }
                }
                Err(_) => complete = false,
            },
            Ok(resp) if matches!(resp.status, 404 | 410) => {}
            _ => complete = false,
        }
    }
    Ok(complete)
}
pub async fn crawl(store: &Store, r: &CrawlRequest) -> Result<Report> {
    crawl_with(store, r, &Fetcher::default()).await
}
pub async fn crawl_with(store: &Store, r: &CrawlRequest, fetch: &impl Transport) -> Result<Report> {
    crawl_with_local_s02_s03(store, r, fetch, false).await
}
/// Explicitly opt in to a local-only sidecar. Callers must persist local execution
/// mode before invoking this and prevent local jobs from being delivered.
pub(crate) async fn crawl_with_local_s02_s03(
    store: &Store,
    r: &CrawlRequest,
    fetch: &impl Transport,
    collect: bool,
) -> Result<Report> {
    let id = r.request_id.to_string();
    let started_at = store.started(&id).await?;
    let started = Instant::now();
    let deadline = chrono::DateTime::parse_from_rfc3339(&started_at)?.timestamp()
        + i64::try_from(r.max_duration_seconds)?;
    let mut policies = HashMap::new();
    let Ok(policy) = robots(fetch, &r.url).await else {
        return report(store, r, started_at, "failed", "robots_unavailable", false).await;
    };
    let map_complete = match tokio::time::timeout(
        Duration::from_secs(60),
        discover_sitemaps(store, r, fetch, &policy.body, collect),
    )
    .await
    {
        Ok(v) => v?,
        Err(_) => false,
    };
    policies.insert(origin(&r.url), policy);
    let existing = store.pages(&id).await?;
    let mut count = existing.iter().filter(|p| !p.is_resource).count();
    let mut resources = existing.iter().filter(|p| p.is_resource).count();
    let mut reason = "frontier_exhausted";
    while let Some((url, depth, in_sitemap, is_resource)) = store
        .next_budgeted(&id, count < r.max_pages, resources < r.max_resources)
        .await?
    {
        if store.cancelled(&id).await? {
            reason = "cancelled";
            break;
        }
        if chrono::Utc::now().timestamp() >= deadline
            || started.elapsed().as_secs() >= r.max_duration_seconds
        {
            reason = "time_limit";
            break;
        }
        let u = Url::parse(&url)?;
        if let std::collections::hash_map::Entry::Vacant(e) = policies.entry(origin(&u)) {
            if let Ok(p) = robots(fetch, &u).await {
                e.insert(p);
            } else {
                reason = "robots_unavailable";
                break;
            }
        }
        let mut page = Page {
            url: url.clone(),
            depth,
            in_sitemap,
            ..Page::default()
        };
        let mut sidecar = crate::collection_evidence::PageCapture::unavailable(&url, "not_fetched");
        if policies[&origin(&u)].allowed(&u) {
            tokio::time::sleep(Duration::from_millis(r.delay_ms)).await;
            match fetch.get(&u).await {
                Ok(response) => {
                    if r.in_scope(&u) || is_resource {
                        page = parse::parse(&u, depth, in_sitemap, &response);
                        if collect {
                            sidecar = crate::collection_evidence::PageCapture::from_response(
                                &url,
                                &response,
                                page.html(),
                            );
                        }
                    } else {
                        page.status = Some(response.status);
                        page.bytes = response.body.len();
                        page.ttfb_ms = response.ttfb_ms;
                        page.content_type = response
                            .headers
                            .get("content-type")
                            .cloned()
                            .unwrap_or_default();
                        page.redirect = response
                            .headers
                            .get("location")
                            .and_then(|v| normalize(&u, v))
                            .map(Into::into);
                    }
                }
                Err(_) => {
                    page.error = Some("fetch_failed_or_destination_denied".into());
                    sidecar.state = "fetch_failed".into();
                }
            }
        } else {
            page.blocked_by_robots = true;
            sidecar.state = "robots_blocked".into();
        }
        if collect {
            let policy = &policies[&origin(&u)];
            sidecar.robots_source = policy.source.into();
            if policy.body.len() <= 64 * 1024 {
                sidecar.robots = [
                    "Seosmith",
                    "Googlebot",
                    "Bingbot",
                    "GPTBot",
                    "OAI-SearchBot",
                    "ChatGPT-User",
                ]
                .iter()
                .map(|bot| crate::robots_evidence::evaluate(&policy.body, bot, &u))
                .collect();
            } else {
                sidecar.robots_source = "policy_limit".into();
            }
            if policy.source != "robots_txt" {
                for decision in &mut sidecar.robots {
                    decision.matched_line = None;
                    decision.rule = None;
                    decision.provenance = policy.source.into();
                }
            }
        }
        page.is_resource = is_resource;
        let follow = !page
            .robots
            .to_ascii_lowercase()
            .split([',', ' ', ';'])
            .any(|d| matches!(d, "nofollow" | "none"));
        let cap = (r.max_pages + r.max_resources).saturating_mul(10);
        if r.in_scope(&u) && !is_resource {
            for link in page
                .links
                .iter()
                .filter(|l| follow && !l.nofollow && !l.sponsored && !l.ugc)
            {
                if let Some(target) =
                    normalize(&u, &link.url).filter(|u| r.in_scope(u) || r.check_external_links)
                {
                    store
                        .discover_kind(
                            &id,
                            target.as_str(),
                            depth + 1,
                            false,
                            cap,
                            !r.in_scope(&target),
                        )
                        .await?;
                }
            }
        }
        if let Some(target) = page
            .redirect
            .as_deref()
            .and_then(|v| normalize(&u, v))
            .filter(|t| r.in_scope(t) || is_resource)
        {
            store
                .discover_kind(&id, target.as_str(), depth + 1, false, cap, is_resource)
                .await?;
        }
        for relation in &page.relationships {
            if relation.kind == "navigation" {
                continue;
            }
            if is_resource
                && (!relation.resource
                    || !page
                        .content_type
                        .to_ascii_lowercase()
                        .starts_with("text/css"))
            {
                continue;
            }
            if let Some(target) =
                normalize(&u, &relation.url).filter(|t| relation.resource || r.in_scope(t))
            {
                store
                    .discover_kind(
                        &id,
                        target.as_str(),
                        depth + 1,
                        false,
                        cap,
                        relation.resource,
                    )
                    .await?;
            }
        }
        // Checkpoint after discoveries: a crash can replay discovery without losing targets.
        if collect {
            store.save_page_with_s02_s03(&id, &page, &sidecar).await?;
        } else {
            store.save_page(&id, &page).await?;
        }
        if is_resource {
            resources += 1;
        } else {
            count += 1;
        }
    }
    if reason == "frontier_exhausted" && store.next(&id).await?.is_some() {
        reason = if store.next_budgeted(&id, true, false).await?.is_some() {
            "page_limit"
        } else {
            "resource_limit"
        };
    }
    let pages = store.pages(&id).await?;
    if store.cancelled(&id).await? {
        reason = "cancelled";
    }
    let truncated = pages
        .iter()
        .any(|p| p.flags.iter().any(|f| f == "extraction_limit"))
        || store.discovery_truncated(&id).await?;
    if pages.iter().any(|p| p.html() && p.encoding.is_empty()) && reason == "frontier_exhausted" {
        reason = "legacy_checkpoint_metadata";
    }
    if truncated && reason == "frontier_exhausted" {
        reason = "discovery_or_extraction_limit";
    }
    let complete =
        reason == "frontier_exhausted" && map_complete && !pages.iter().any(|p| p.error.is_some());
    let outcome = if reason == "cancelled" {
        "cancelled"
    } else if complete {
        "completed"
    } else {
        "partial"
    };
    let reason = if reason == "frontier_exhausted" && !map_complete {
        "sitemap_incomplete"
    } else if reason == "frontier_exhausted" && pages.iter().any(|p| p.error.is_some()) {
        "fetch_failures"
    } else {
        reason
    };
    report(store, r, started_at, outcome, reason, complete).await
}
async fn report(
    store: &Store,
    r: &CrawlRequest,
    started_at: String,
    outcome: &str,
    reason: &str,
    complete: bool,
) -> Result<Report> {
    let mut pages = store.pages(&r.request_id.to_string()).await?;
    let states: HashMap<_, _> = pages
        .iter()
        .map(|p| {
            (
                p.url.clone(),
                if p.blocked_by_robots {
                    "blocked"
                } else if p.error.is_some() {
                    "fetch_failed"
                } else {
                    "checked"
                },
            )
        })
        .collect();
    for page in &mut pages {
        for relation in &mut page.relationships {
            relation.state = states
                .get(&relation.url)
                .map_or_else(
                    || {
                        if !relation.resource
                            && Url::parse(&relation.url).is_ok_and(|u| !r.in_scope(&u))
                        {
                            "outside_scope"
                        } else {
                            "not_fetched"
                        }
                    },
                    |s| *s,
                )
                .into();
        }
    }
    let findings = audit::analyze(&pages, &r.url);
    Ok(Report {
        schema_version: model::SCHEMA_VERSION,
        engine_version: model::ENGINE_VERSION.into(),
        ruleset_version: model::RULESET_VERSION.into(),
        request: r.clone(),
        started_at,
        finished_at: chrono::Utc::now().to_rfc3339(),
        outcome: outcome.into(),
        stop_reason: reason.into(),
        coverage_complete: complete,
        pages,
        findings,
        evidence_contract: None,
        policy_guidance: None,
    })
}

#[cfg(test)]
mod local_capture_tests {
    use super::*;
    use crate::fetch::Response;
    use std::collections::BTreeMap;
    struct Fixture;
    impl Transport for Fixture {
        async fn get(&self, url: &Url) -> Result<Response> {
            let (status, body) = match url.path() {
                "/robots.txt" => (200, "User-agent: *\nDisallow: /blocked"),
                "/sitemap.xml" => (
                    200,
                    "<urlset><url><loc>https://example.com/blocked</loc><lastmod>2026-01-01</lastmod></url></urlset>",
                ),
                _ => (
                    200,
                    "<html><head><meta name='Googlebot' content='noindex'><link rel='canonical' href=''></head></html>",
                ),
            };
            Ok(Response {
                status,
                headers: BTreeMap::from([("content-type".into(), "text/html".into())]),
                raw_headers: Some(vec![
                    ("x-robots-tag".into(), "Googlebot: noindex".into()),
                    ("x-robots-tag".into(), "Googlebot: index".into()),
                ]),
                body: body.as_bytes().to_vec(),
                ttfb_ms: 1,
            })
        }
    }
    #[tokio::test]
    async fn opt_in_checkpoints_observed_and_blocked_separately() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("db").to_str().unwrap())
            .await
            .unwrap();
        let r = CrawlRequest {
            request_id: uuid::Uuid::new_v4(),
            website_id: uuid::Uuid::new_v4(),
            binding_generation: 1,
            url: Url::parse("https://example.com/").unwrap(),
            max_pages: 10,
            max_duration_seconds: 20,
            delay_ms: 1000,
            check_external_links: false,
            max_resources: 0,
        };
        store.submit(&r).await.unwrap();
        store.claim().await.unwrap();
        let report = crawl_with_local_s02_s03(&store, &r, &Fixture, true)
            .await
            .unwrap();
        assert_eq!(report.schema_version, 1);
        let id = r.request_id.to_string();
        let seed = store
            .s02_s03_page(&id, r.url.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seed.http.unwrap().occurrences.len(), 2);
        assert_eq!(seed.html.unwrap().occurrences.len(), 2);
        let blocked = store
            .s02_s03_page(&id, "https://example.com/blocked")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(blocked.state, "robots_blocked");
        assert!(blocked.html.is_none());
        let sitemap: String = sqlx::query_scalar(
            "SELECT evidence FROM local_s02_s03_sitemaps WHERE job_id=? AND url=?",
        )
        .bind(id)
        .bind("https://example.com/sitemap.xml")
        .fetch_one(&store.0)
        .await
        .unwrap();
        assert!(sitemap.contains("2026-01-01"));
    }
}
