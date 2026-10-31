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
        200 => Ok(Robots {
            body: String::from_utf8_lossy(&r.body).into_owned(),
        }),
        404 | 410 => Ok(Robots {
            body: String::new(),
        }),
        401 | 403 => Ok(Robots {
            body: "User-agent: *\nDisallow: /".into(),
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
        if !(Robots { body: body.into() }).allowed(&u) {
            complete = false;
            continue;
        }
        if !seen.insert(u.to_string()) {
            continue;
        }
        if seen.len() > 20 {
            complete = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(r.delay_ms)).await;
        match fetch.get(&u).await {
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
        discover_sitemaps(store, r, fetch, &policy.body),
    )
    .await
    {
        Ok(v) => v?,
        Err(_) => false,
    };
    policies.insert(origin(&r.url), policy);
    let mut count = store.pages(&id).await?.len();
    let mut reason = "frontier_exhausted";
    while let Some((url, depth, in_sitemap)) = store.next(&id).await? {
        if store.cancelled(&id).await? {
            reason = "cancelled";
            break;
        }
        if count >= r.max_pages {
            reason = "page_limit";
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
        if policies[&origin(&u)].allowed(&u) {
            tokio::time::sleep(Duration::from_millis(r.delay_ms)).await;
            match fetch.get(&u).await {
                Ok(response) => {
                    if r.in_scope(&u) {
                        page = parse::parse(&u, depth, in_sitemap, &response);
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
                Err(_) => page.error = Some("fetch_failed_or_destination_denied".into()),
            }
        } else {
            page.blocked_by_robots = true;
        }
        let follow = !page
            .robots
            .to_ascii_lowercase()
            .split([',', ' ', ';'])
            .any(|d| matches!(d, "nofollow" | "none"));
        if r.in_scope(&u) {
            for image in &page.images {
                if let Some(target) = normalize(&u, &image.url).filter(|u| r.in_scope(u)) {
                    store
                        .discover(&id, target.as_str(), depth + 1, false, r.max_pages * 10)
                        .await?;
                }
            }
            for link in page
                .links
                .iter()
                .filter(|l| follow && !l.nofollow)
                .map(|l| l.url.as_str())
                .chain(page.redirect.as_deref())
            {
                if let Some(target) = normalize(&u, link).filter(|u| {
                    r.in_scope(u)
                        || (r.check_external_links
                            && page.links.iter().any(|l| l.url == u.as_str()))
                }) {
                    store
                        .discover(&id, target.as_str(), depth + 1, false, r.max_pages * 10)
                        .await?;
                }
            }
        }
        // Checkpoint after discoveries: a crash can replay discovery without losing targets.
        store.save_page(&id, &page).await?;
        count += 1;
    }
    let pages = store.pages(&id).await?;
    if store.cancelled(&id).await? {
        reason = "cancelled";
    }
    let truncated = pages
        .iter()
        .any(|p| p.flags.iter().any(|f| f == "extraction_limit"))
        || store.discovery_truncated(&id).await?;
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
    let pages = store.pages(&r.request_id.to_string()).await?;
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
    })
}
