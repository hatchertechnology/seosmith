//! Experimental, local-only evidence. This module is not part of any delivered report.
use crate::model::digest;
use scraper::{Html, Selector};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use url::Url;

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}
fn words(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn text(el: scraper::ElementRef<'_>) -> String {
    words(&el.text().collect::<Vec<_>>().join(" "))
}
fn fetch_url(mut url: Url) -> String {
    url.set_fragment(None);
    url.to_string()
}

#[derive(Clone, Debug, Serialize)]
pub struct LinkEvidence {
    pub raw_href: String,
    pub resolved_url: String,
    pub fetch_url: String,
    pub fragment: Option<String>,
    pub accessible_name: String,
    pub element: String,
    pub context: String,
    pub followable: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct ContentEvidence {
    pub exact_text_hash: String,
    pub fingerprint: Vec<String>,
    pub excerpt: String,
    pub main_text_words: usize,
    pub body_text_words: usize,
    pub template_proportion: Option<f64>,
    pub title: String,
    pub h1: Vec<String>,
    pub soft_404_signals: Vec<String>,
    pub review_only: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct ResourceReference {
    pub element: String,
    pub raw_src: Option<String>,
    /// The original srcset is preserved; no browser candidate selection is inferred.
    pub raw_srcset: Option<String>,
    pub sizes: Option<String>,
    pub loading: Option<String>,
    pub fetchpriority: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct LocalPageEvidence {
    pub url: String,
    pub links: Vec<LinkEvidence>,
    pub resource_references: Vec<ResourceReference>,
    pub ids: BTreeSet<String>,
    pub content: ContentEvidence,
    pub extraction_complete: bool,
}
/// Extract bounded source evidence from a decoded HTML response. It makes no policy judgment.
pub fn extract(url: &Url, html: &str, status: u16) -> LocalPageEvidence {
    let doc = Html::parse_document(html);
    let base = doc
        .select(&selector("base[href]"))
        .next()
        .and_then(|e| url.join(e.value().attr("href")?).ok())
        .filter(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.username().is_empty()
                && u.password().is_none()
        })
        .unwrap_or_else(|| url.clone());
    let mut links = Vec::new();
    let mut complete = true;
    for el in doc.select(&selector("a[href],area[href]")) {
        if links.len() == 2000 {
            complete = false;
            break;
        }
        let raw = el.value().attr("href").unwrap_or_default();
        let Ok(target) = base.join(raw.trim()) else {
            continue;
        };
        if !matches!(target.scheme(), "http" | "https")
            || !target.username().is_empty()
            || target.password().is_some()
        {
            continue;
        }
        let rel = el
            .value()
            .attr("rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let context = el
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .find_map(|a| match a.value().name() {
                "nav" | "header" | "footer" | "main" | "article" => {
                    Some(a.value().name().to_owned())
                }
                _ => None,
            })
            .unwrap_or_else(|| "other".into());
        let name = el
            .value()
            .attr("aria-label")
            .map(words)
            .filter(|s| !s.is_empty())
            .or_else(|| el.value().attr("alt").map(words).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| text(el));
        links.push(LinkEvidence {
            raw_href: raw.chars().take(2048).collect(),
            resolved_url: target.to_string(),
            fetch_url: fetch_url(target.clone()),
            fragment: target.fragment().map(str::to_owned),
            accessible_name: name.chars().take(256).collect(),
            element: el.value().name().into(),
            context,
            followable: !rel
                .split_whitespace()
                .any(|x| matches!(x, "nofollow" | "sponsored" | "ugc")),
        });
    }
    let mut resource_references = Vec::new();
    for el in doc.select(&selector("img,source,script[src],link[href]")) {
        if resource_references.len() == 2000 {
            complete = false;
            break;
        }
        if [
            "src",
            "href",
            "srcset",
            "sizes",
            "loading",
            "fetchpriority",
            "width",
            "height",
        ]
        .iter()
        .filter_map(|name| el.value().attr(name))
        .any(|v| v.chars().count() > 2048)
        {
            complete = false;
        }
        let attr = |name| {
            el.value()
                .attr(name)
                .map(|v| v.chars().take(2048).collect())
        };
        resource_references.push(ResourceReference {
            element: el.value().name().into(),
            raw_src: attr("src").or_else(|| attr("href")),
            raw_srcset: attr("srcset"),
            sizes: attr("sizes"),
            loading: attr("loading"),
            fetchpriority: attr("fetchpriority"),
            width: attr("width"),
            height: attr("height"),
        });
    }
    let id_count = doc.select(&selector("[id],a[name]")).count();
    if id_count > 10000 {
        complete = false;
    }
    let ids = doc
        .select(&selector("[id],a[name]"))
        .filter_map(|e| e.value().attr("id").or_else(|| e.value().attr("name")))
        .take(10000)
        .map(str::to_owned)
        .collect();
    let body = doc
        .select(&selector("body"))
        .next()
        .map_or_else(String::new, |e| visible_text(e, false));
    let main = doc
        .select(&selector("main,article,[role=main]"))
        .next()
        .map_or_else(|| body.clone(), |e| visible_text(e, true));
    let body_count = body.split_whitespace().count();
    let main_count = main.split_whitespace().count();
    let mut fingerprint: Vec<_> = main
        .to_lowercase()
        .split_whitespace()
        .filter(|s| s.chars().count() > 2)
        .map(str::to_owned)
        .collect();
    fingerprint.sort();
    fingerprint.dedup();
    fingerprint.truncate(512);
    let title = doc
        .select(&selector("head title"))
        .next()
        .map_or_else(String::new, text);
    let h1: Vec<_> = doc.select(&selector("h1")).take(20).map(text).collect();
    let mut signals = Vec::new();
    let label = format!("{} {}", title, h1.join(" ")).to_lowercase();
    if status == 200
        && ["404", "page not found", "not found", "does not exist"]
            .iter()
            .any(|p| label.contains(p))
    {
        signals.push("error_title_or_heading".into());
    }
    if status == 200 && main_count == 0 {
        signals.push("empty_static_main_text".into());
    }
    LocalPageEvidence {
        url: fetch_url(url.clone()),
        links,
        resource_references,
        ids,
        extraction_complete: complete,
        content: ContentEvidence {
            exact_text_hash: digest(main.as_bytes()),
            fingerprint,
            excerpt: main.chars().take(256).collect(),
            main_text_words: main_count,
            body_text_words: body_count,
            template_proportion: u32::try_from(body_count).ok().filter(|&n| n > 0).and_then(
                |body| {
                    u32::try_from(main_count.min(body_count))
                        .ok()
                        .map(|main| 1.0 - f64::from(main) / f64::from(body))
                },
            ),
            title,
            h1,
            soft_404_signals: signals,
            review_only: true,
        },
    }
}
fn visible_text(el: scraper::ElementRef<'_>, _: bool) -> String {
    words(
        &el.descendants()
            .filter_map(|n| {
                let t = n.value().as_text()?;
                (!n.ancestors()
                    .filter_map(scraper::ElementRef::wrap)
                    .any(|a| {
                        matches!(
                            a.value().name(),
                            "script" | "style" | "noscript" | "template"
                        )
                    }))
                .then(|| t.to_string())
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}
#[derive(Debug, Serialize)]
pub struct GraphNode {
    pub url: String,
    pub unique_inlinks: usize,
    pub navigation_distance: Option<usize>,
    pub reachability: &'static str,
}
/// Navigation distances use only observed followable links, never discovery depth.
pub fn graph(seed: &str, pages: &[LocalPageEvidence], crawl_complete: bool) -> Vec<GraphNode> {
    let by_url: BTreeMap<_, _> = pages.iter().map(|p| (p.url.as_str(), p)).collect();
    let mut incoming: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for page in pages {
        for link in &page.links {
            if by_url.contains_key(link.fetch_url.as_str()) {
                incoming
                    .entry(&link.fetch_url)
                    .or_default()
                    .insert(&page.url);
            }
        }
    }
    let mut distances = BTreeMap::new();
    let mut queue = VecDeque::new();
    if by_url.contains_key(seed) {
        distances.insert(seed, 0usize);
        queue.push_back(seed);
    }
    while let Some(current) = queue.pop_front() {
        let d = distances[current];
        for link in &by_url[current].links {
            let target = link.fetch_url.as_str();
            if link.followable && by_url.contains_key(target) && !distances.contains_key(target) {
                distances.insert(target, d + 1);
                queue.push_back(target);
            }
        }
    }
    pages
        .iter()
        .map(|p| {
            let distance = distances.get(p.url.as_str()).copied();
            GraphNode {
                url: p.url.clone(),
                unique_inlinks: incoming.get(p.url.as_str()).map_or(0, BTreeSet::len),
                navigation_distance: distance,
                reachability: if distance.is_some() {
                    "reachable"
                } else if crawl_complete && pages.iter().all(|p| p.extraction_complete) {
                    "not_observed_reachable"
                } else {
                    "unknown"
                },
            }
        })
        .collect()
}
/// Jaccard similarity of bounded unique tokens; candidates require human review.
pub fn similarity(a: &ContentEvidence, b: &ContentEvidence) -> Option<f64> {
    if a.fingerprint.is_empty() || b.fingerprint.is_empty() {
        return None;
    }
    let a: BTreeSet<_> = a.fingerprint.iter().collect();
    let b: BTreeSet<_> = b.fingerprint.iter().collect();
    Some(
        f64::from(u32::try_from(a.intersection(&b).count()).ok()?)
            / f64::from(u32::try_from(a.union(&b).count()).ok()?),
    )
}

#[derive(Debug, Serialize)]
pub struct ResourceEvidence {
    pub url: String,
    pub decoded_body_bytes: usize,
    /// A header is an estimate of the encoded representation, not a measured transfer.
    pub declared_content_length: Option<u64>,
    pub transfer_bytes: Option<u64>,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub cache_control: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body_hash: String,
    pub detected_image_format: Option<&'static str>,
    pub image_dimensions: Option<(u32, u32)>,
}
/// Body is the decoded bytes returned by the transport. Transfer size is unavailable.
pub fn resource(url: &str, headers: &BTreeMap<String, String>, body: &[u8]) -> ResourceEvidence {
    let (format, dimensions) = image_info(body);
    ResourceEvidence {
        url: url.into(),
        decoded_body_bytes: body.len(),
        declared_content_length: headers.get("content-length").and_then(|v| v.parse().ok()),
        transfer_bytes: None,
        content_type: headers.get("content-type").cloned(),
        content_encoding: headers.get("content-encoding").cloned(),
        cache_control: headers.get("cache-control").cloned(),
        etag: headers.get("etag").cloned(),
        last_modified: headers.get("last-modified").cloned(),
        body_hash: digest(body),
        detected_image_format: format,
        image_dimensions: dimensions,
    }
}
fn image_info(b: &[u8]) -> (Option<&'static str>, Option<(u32, u32)>) {
    if b.len() >= 24 && b.starts_with(b"\x89PNG\r\n\x1a\n") && &b[12..16] == b"IHDR" {
        return (
            Some("png"),
            Some((
                u32::from_be_bytes(b[16..20].try_into().unwrap()),
                u32::from_be_bytes(b[20..24].try_into().unwrap()),
            )),
        );
    }
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        return (
            Some("gif"),
            Some((
                u16::from_le_bytes([b[6], b[7]]).into(),
                u16::from_le_bytes([b[8], b[9]]).into(),
            )),
        );
    }
    if b.starts_with(&[0xff, 0xd8, 0xff]) {
        return (Some("jpeg"), None);
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return (Some("webp"), None);
    }
    (None, None)
}

#[derive(Debug, Serialize)]
pub struct FragmentCheck {
    pub source: String,
    pub target: String,
    pub fragment: String,
    pub state: &'static str,
}
/// Encoded fragments remain unknown until a matching browser-compatible decoder is used.
pub fn fragment_checks(pages: &[LocalPageEvidence]) -> Vec<FragmentCheck> {
    let by_url: BTreeMap<_, _> = pages.iter().map(|p| (p.url.as_str(), p)).collect();
    let mut checks = Vec::new();
    for p in pages {
        for link in &p.links {
            if let Some(fragment) = &link.fragment {
                let state = if fragment.is_empty() {
                    "top"
                } else if fragment.contains('%') {
                    "unknown_encoded"
                } else if let Some(target) = by_url.get(link.fetch_url.as_str()) {
                    if target.ids.contains(fragment) {
                        "present"
                    } else if target.extraction_complete {
                        "absent"
                    } else {
                        "unknown_truncated"
                    }
                } else {
                    "unknown_target"
                };
                checks.push(FragmentCheck {
                    source: p.url.clone(),
                    target: link.fetch_url.clone(),
                    fragment: fragment.clone(),
                    state,
                });
            }
        }
    }
    checks
}
/// Propagate static resource dependencies back to HTML pages, with cycles bounded by visited URLs.
pub fn affected_pages(pages: &[crate::model::Page]) -> BTreeMap<String, BTreeSet<String>> {
    let by_url: BTreeMap<_, _> = pages.iter().map(|p| (p.url.as_str(), p)).collect();
    let mut affected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for page in pages.iter().filter(|p| !p.is_resource && p.html()) {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<&str> = page
            .relationships
            .iter()
            .filter(|r| r.resource)
            .map(|r| r.url.as_str())
            .collect();
        while let Some(target) = queue.pop_front() {
            if !seen.insert(target) {
                continue;
            }
            affected
                .entry(target.to_owned())
                .or_default()
                .insert(page.url.clone());
            if let Some(resource) = by_url.get(target) {
                queue.extend(
                    resource
                        .relationships
                        .iter()
                        .filter(|r| r.resource)
                        .map(|r| r.url.as_str()),
                );
            }
        }
    }
    affected
}

#[derive(Debug, Serialize)]
pub struct SimilarityCandidate {
    pub first: String,
    pub second: String,
    pub similarity: f64,
    pub review_only: bool,
}
#[derive(Debug, Serialize)]
pub struct CandidateSet {
    pub candidates: Vec<SimilarityCandidate>,
    pub comparisons: usize,
    pub complete: bool,
}
/// Bounded pairwise candidates. Short texts are excluded rather than judged dissimilar.
pub fn similarity_candidates(
    pages: &[LocalPageEvidence],
    minimum_words: usize,
    threshold: f64,
    max_comparisons: usize,
) -> CandidateSet {
    let mut result = CandidateSet {
        candidates: Vec::new(),
        comparisons: 0,
        complete: true,
    };
    for (i, a) in pages.iter().enumerate() {
        for b in &pages[i + 1..] {
            if a.content.main_text_words < minimum_words
                || b.content.main_text_words < minimum_words
            {
                continue;
            }
            if result.comparisons == max_comparisons {
                result.complete = false;
                return result;
            }
            result.comparisons += 1;
            if let Some(score) = similarity(&a.content, &b.content)
                && score >= threshold
            {
                result.candidates.push(SimilarityCandidate {
                    first: a.url.clone(),
                    second: b.url.clone(),
                    similarity: score,
                    review_only: true,
                });
            }
        }
    }
    result
}

#[derive(Debug, Serialize)]
pub struct CanonicalVariantLink {
    pub source: String,
    pub link_ordinal: usize,
    pub target: String,
    pub declared_canonical: String,
}
/// Reports observed links to fetched pages declaring a different canonical; not a validation of that canonical.
pub fn canonical_variant_links(
    local: &[LocalPageEvidence],
    pages: &[crate::model::Page],
) -> Vec<CanonicalVariantLink> {
    let canonical: BTreeMap<_, _> = pages
        .iter()
        .filter(|p| p.html())
        .filter_map(|p| {
            p.canonical
                .as_ref()
                .filter(|c| *c != &p.url)
                .map(|c| (p.url.as_str(), c.as_str()))
        })
        .collect();
    local
        .iter()
        .flat_map(|source| {
            source
                .links
                .iter()
                .enumerate()
                .filter_map(|(ordinal, link)| {
                    canonical
                        .get(link.fetch_url.as_str())
                        .map(|target| CanonicalVariantLink {
                            source: source.url.clone(),
                            link_ordinal: ordinal,
                            target: link.fetch_url.clone(),
                            declared_canonical: (*target).into(),
                        })
                })
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct DuplicateResourceGroup {
    pub decoded_body_hash: String,
    pub urls: Vec<String>,
}
/// Equality of decoded bodies is an observation, not proof that assets are interchangeable.
pub fn duplicate_resources(resources: &[ResourceEvidence]) -> Vec<DuplicateResourceGroup> {
    let mut groups: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in resources {
        groups.entry(&r.body_hash).or_default().insert(&r.url);
    }
    groups
        .into_iter()
        .filter(|(_, urls)| urls.len() > 1)
        .map(|(hash, urls)| DuplicateResourceGroup {
            decoded_body_hash: hash.into(),
            urls: urls.into_iter().map(str::to_owned).collect(),
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct ResourceByteGroup {
    pub origin: Option<String>,
    pub content_type: Option<String>,
    pub resource_count: usize,
    pub decoded_body_bytes: u64,
    pub complete: bool,
}
/// Totals only observed decoded bodies, grouped by origin and declared MIME type.
pub fn resource_byte_groups(
    resources: &[ResourceEvidence],
    collection_complete: bool,
) -> Vec<ResourceByteGroup> {
    type ByteGroupMap = BTreeMap<(Option<String>, Option<String>), (usize, u64, bool)>;
    let mut groups: ByteGroupMap = BTreeMap::new();
    for r in resources {
        let origin = Url::parse(&r.url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .map(|u| u.origin().ascii_serialization());
        let content_type = r
            .content_type
            .as_ref()
            .map(|s| {
                s.split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            })
            .filter(|s| !s.is_empty());
        let group = groups
            .entry((origin, content_type))
            .or_insert((0, 0, collection_complete));
        group.0 += 1;
        match u64::try_from(r.decoded_body_bytes)
            .ok()
            .and_then(|n| group.1.checked_add(n))
        {
            Some(sum) => group.1 = sum,
            None => group.2 = false,
        }
    }
    groups
        .into_iter()
        .map(
            |((origin, content_type), (resource_count, decoded_body_bytes, complete))| {
                ResourceByteGroup {
                    origin,
                    content_type,
                    resource_count,
                    decoded_body_bytes,
                    complete,
                }
            },
        )
        .collect()
}

/// Inputs come from the durable run record, not inferred from an empty sidecar.
#[derive(Debug)]
pub struct GraphCoverageInput {
    pub discovery_complete: Option<bool>,
    pub expected_pages: Option<usize>,
    pub observed_pages: usize,
    pub unavailable_pages: usize,
    pub truncated_pages: usize,
    pub omitted_records: Option<usize>,
    pub comparisons_complete: Option<bool>,
}
#[derive(Debug, Serialize)]
pub struct GraphCoverage {
    pub complete: bool,
    pub reasons: Vec<&'static str>,
}
/// Unknown and contradictory counts never establish complete coverage.
pub fn graph_coverage(input: &GraphCoverageInput) -> GraphCoverage {
    let mut reasons = Vec::new();
    if input.discovery_complete != Some(true) {
        reasons.push("discovery_incomplete_or_unknown");
    }
    match input.expected_pages {
        None => reasons.push("page_population_unknown"),
        Some(expected)
            if input.observed_pages.checked_add(input.unavailable_pages) != Some(expected) =>
        {
            reasons.push("page_counts_unreconciled");
        }
        Some(_) => {}
    }
    if input.unavailable_pages > 0 {
        reasons.push("pages_unavailable");
    }
    if input.truncated_pages > input.observed_pages {
        reasons.push("truncation_counts_invalid");
    } else if input.truncated_pages > 0 {
        reasons.push("extraction_truncated");
    }
    if input.omitted_records != Some(0) {
        reasons.push("records_omitted_or_unknown");
    }
    if input.comparisons_complete != Some(true) {
        reasons.push("comparisons_incomplete_or_unknown");
    }
    GraphCoverage {
        complete: reasons.is_empty(),
        reasons,
    }
}

#[derive(Debug, Serialize)]
pub struct SimilarityCluster {
    pub urls: Vec<String>,
    /// Connected components are review groups; not every pair necessarily exceeds the threshold.
    pub review_only: bool,
}
/// Groups only observed candidate edges. Incomplete comparison sets remain incomplete.
pub fn similarity_clusters(candidates: &CandidateSet) -> (Vec<SimilarityCluster>, bool) {
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for c in &candidates.candidates {
        edges.entry(&c.first).or_default().insert(&c.second);
        edges.entry(&c.second).or_default().insert(&c.first);
    }
    let mut visited = BTreeSet::new();
    let mut clusters = Vec::new();
    for &start in edges.keys() {
        if !visited.insert(start) {
            continue;
        }
        let mut members = BTreeSet::new();
        let mut pending = VecDeque::from([start]);
        while let Some(url) = pending.pop_front() {
            members.insert(url);
            for &next in &edges[url] {
                if visited.insert(next) {
                    pending.push_back(next);
                }
            }
        }
        clusters.push(SimilarityCluster {
            urls: members.into_iter().map(str::to_owned).collect(),
            review_only: true,
        });
    }
    (clusters, candidates.complete)
}
