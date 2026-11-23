//! Experimental, local-only evidence. This module is not part of any delivered report.
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
pub struct LocalPageEvidence {
    pub url: String,
    pub links: Vec<LinkEvidence>,
    pub ids: BTreeSet<String>,
    pub extraction_complete: bool,
}
/// Extract link evidence from supplied HTML.
pub fn extract(url: &Url, html: &str, _status: u16) -> LocalPageEvidence {
    let doc = Html::parse_document(html);
    let mut complete = true;
    let base = if let Some(element) = doc.select(&selector("base[href]")).next() {
        match element
            .value()
            .attr("href")
            .and_then(|href| url.join(href).ok())
        {
            Some(base)
                if matches!(base.scheme(), "http" | "https")
                    && base.username().is_empty()
                    && base.password().is_none() =>
            {
                base
            }
            _ => {
                complete = false;
                url.clone()
            }
        }
    } else {
        url.clone()
    };
    let mut links = Vec::new();
    for el in doc.select(&selector("a[href],area[href]")) {
        if links.len() == 2000 {
            complete = false;
            break;
        }
        let raw = el.value().attr("href").unwrap_or_default();
        if raw.chars().count() > 2048 {
            complete = false;
        }
        let Ok(target) = base.join(raw.trim()) else {
            complete = false;
            continue;
        };
        if !matches!(target.scheme(), "http" | "https")
            || !target.username().is_empty()
            || target.password().is_some()
        {
            complete = false;
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
    let id_count = doc.select(&selector("[id],a[name]")).count();
    if id_count > 10000 {
        complete = false;
    }
    let ids = doc
        .select(&selector("[id],a[name]"))
        .take(10000)
        .flat_map(|e| {
            [
                e.value().attr("id"),
                (e.value().name() == "a")
                    .then(|| e.value().attr("name"))
                    .flatten(),
            ]
        })
        .flatten()
        .map(str::to_owned)
        .collect();
    LocalPageEvidence {
        url: fetch_url(url.clone()),
        links,
        ids,
        extraction_complete: complete,
    }
}
/// Duplicate URL observations require caller reconciliation before analysis.
#[derive(Debug, PartialEq, Eq)]
pub struct DuplicatePageUrl(pub String);

fn unique_pages(
    pages: &[LocalPageEvidence],
) -> Result<BTreeMap<&str, &LocalPageEvidence>, DuplicatePageUrl> {
    let mut by_url = BTreeMap::new();
    for page in pages {
        if by_url.insert(page.url.as_str(), page).is_some() {
            return Err(DuplicatePageUrl(page.url.clone()));
        }
    }
    Ok(by_url)
}
#[derive(Debug, Serialize)]
pub struct GraphNode {
    pub url: String,
    pub unique_inlinks: usize,
    pub navigation_distance: Option<usize>,
    pub reachability: &'static str,
}
/// Navigation distances use only observed followable links, never discovery depth.
pub fn graph(
    seed: &str,
    pages: &[LocalPageEvidence],
    coverage: &GraphCoverageInput,
) -> Result<Vec<GraphNode>, DuplicatePageUrl> {
    let by_url = unique_pages(pages)?;
    let coverage_complete = graph_coverage(coverage).complete
        && coverage.observed_pages == pages.len()
        && coverage.truncated_pages == pages.iter().filter(|p| !p.extraction_complete).count();
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
    Ok(pages
        .iter()
        .map(|p| {
            let distance = distances.get(p.url.as_str()).copied();
            GraphNode {
                url: p.url.clone(),
                unique_inlinks: incoming.get(p.url.as_str()).map_or(0, BTreeSet::len),
                navigation_distance: distance,
                reachability: if distance.is_some() {
                    "reachable"
                } else if coverage_complete
                    && by_url.contains_key(seed)
                    && pages.iter().all(|p| p.extraction_complete)
                {
                    "not_observed_reachable"
                } else {
                    "unknown"
                },
            }
        })
        .collect())
}
#[derive(Debug, Serialize)]
pub struct FragmentCheck {
    pub source: String,
    pub target: String,
    pub fragment: String,
    pub state: &'static str,
}
/// Encoded fragments remain unknown until a matching browser-compatible decoder is used.
pub fn fragment_checks(
    pages: &[LocalPageEvidence],
) -> Result<Vec<FragmentCheck>, DuplicatePageUrl> {
    let by_url = unique_pages(pages)?;
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
                    } else if !target.extraction_complete {
                        "unknown_truncated"
                    } else if fragment.eq_ignore_ascii_case("top") {
                        "top"
                    } else {
                        "absent"
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
    Ok(checks)
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
