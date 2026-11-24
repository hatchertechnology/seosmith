//! Experimental, local-only evidence. This module is not part of any delivered report.
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
        if name.chars().count() > 256 {
            complete = false;
        }
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
    /// Page-level followability from retained directive evidence.
    pub page_follow: &'static str,
    /// Retained sitemap membership, kept separate from navigation.
    pub sitemap_listed: &'static str,
    /// Retained canonical-target declaration evidence, not discovery causation.
    pub canonical_target: &'static str,
}
/// Retained evidence about one supplied page, separate from its link occurrences.
#[derive(Clone, Debug, Default)]
pub struct PageEvidence {
    /// `Some(true)` retained directives permit following this page's links;
    /// `Some(false)` a page-level restriction; `None` unknown or not retained.
    pub page_follow: Option<bool>,
    /// `Some(true)` the URL appears in retained sitemap evidence. Absence stays
    /// unknown because sitemap coverage is not proven here.
    pub sitemap_listed: Option<bool>,
    /// `Some(true)` retained complete canonical evidence names this URL;
    /// `Some(false)` every conclusive retained declaration names another URL.
    pub canonical_target: Option<bool>,
}
/// Navigation distances use only observed followable links, never discovery depth.
/// The seed must exactly match a fragment-free page URL.
pub fn graph(
    seed: &str,
    pages: &[LocalPageEvidence],
    coverage: &GraphCoverageInput,
) -> Result<Vec<GraphNode>, DuplicatePageUrl> {
    graph_impl(seed, pages, coverage, None)
}
/// Navigation with retained page-level evidence. Unlike `graph`, a supplied page
/// with no conclusive followability is treated conservatively: paths through it
/// cannot establish certain reachability and downstream states stay unknown.
pub fn graph_with_evidence(
    seed: &str,
    pages: &[LocalPageEvidence],
    coverage: &GraphCoverageInput,
    evidence: &BTreeMap<String, PageEvidence>,
) -> Result<Vec<GraphNode>, DuplicatePageUrl> {
    graph_impl(seed, pages, coverage, Some(evidence))
}
fn forward(
    by_url: &BTreeMap<&str, &LocalPageEvidence>,
    seed: &str,
    can_traverse: impl Fn(&str) -> bool,
) -> BTreeMap<String, usize> {
    let mut distances = BTreeMap::new();
    if !by_url.contains_key(seed) {
        return distances;
    }
    distances.insert(seed.to_owned(), 0usize);
    let mut queue = VecDeque::from([seed]);
    while let Some(current) = queue.pop_front() {
        let d = distances[current];
        if !can_traverse(current) {
            continue;
        }
        for link in &by_url[current].links {
            let target = link.fetch_url.as_str();
            if link.followable && by_url.contains_key(target) && !distances.contains_key(target) {
                distances.insert(target.to_owned(), d + 1);
                queue.push_back(target);
            }
        }
    }
    distances
}
fn graph_impl(
    seed: &str,
    pages: &[LocalPageEvidence],
    coverage: &GraphCoverageInput,
    evidence: Option<&BTreeMap<String, PageEvidence>>,
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
    // No evidence means the legacy link-level-only contract: observed followable
    // links are navigable and page-level followability is not evaluated.
    let follow_of = |url: &str| -> Option<bool> {
        evidence.map_or(Some(true), |map| {
            map.get(url).and_then(|item| item.page_follow)
        })
    };
    let distances = forward(&by_url, seed, |url| follow_of(url) == Some(true));
    // Over-approximate reachability when page-level followability is unknown but
    // a page-level restriction or link-level nofollow is not observed.
    let possible = evidence.map_or_else(BTreeMap::new, |_| {
        forward(&by_url, seed, |url| follow_of(url) != Some(false))
    });
    Ok(pages
        .iter()
        .map(|p| {
            let url = p.url.as_str();
            let distance = distances.get(url).copied();
            let reachability = if distance.is_some() {
                "reachable"
            } else if possible.contains_key(url) {
                "unknown"
            } else if coverage_complete
                && by_url.contains_key(seed)
                && pages.iter().all(|p| p.extraction_complete)
            {
                "not_observed_reachable"
            } else {
                "unknown"
            };
            let item = evidence.and_then(|map| map.get(url));
            GraphNode {
                url: p.url.clone(),
                unique_inlinks: incoming.get(url).map_or(0, BTreeSet::len),
                navigation_distance: distance,
                reachability,
                page_follow: match item.and_then(|i| i.page_follow) {
                    Some(true) => "followable",
                    Some(false) => "restricted",
                    None => "unknown",
                },
                sitemap_listed: match item.and_then(|i| i.sitemap_listed) {
                    Some(true) => "listed",
                    _ => "unknown",
                },
                canonical_target: match item.and_then(|i| i.canonical_target) {
                    Some(true) => "declared",
                    Some(false) => "not_declared",
                    None => "unknown",
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
    /// Retained declaration origin, for example `html_head_link`.
    pub declaration_source: String,
    /// Evidence semantics version for the declaration origin.
    pub declaration_version: u8,
}
/// One conclusive canonical declaration retained for a supplied page. Only emitted
/// when retained evidence resolves a single target, never as validated truth.
#[derive(Clone, Debug)]
pub struct CanonicalDeclaration {
    pub page: String,
    pub declared_canonical: String,
    pub declaration_source: String,
    pub declaration_version: u8,
}
/// Reports observed links to fetched pages declaring a different canonical; not a validation of that canonical.
pub fn canonical_variant_links(
    local: &[LocalPageEvidence],
    pages: &[crate::model::Page],
) -> Result<Vec<CanonicalVariantLink>, DuplicatePageUrl> {
    unique_pages(local)?;
    let mut seen = BTreeSet::new();
    for page in pages {
        if !seen.insert(page.url.as_str()) {
            return Err(DuplicatePageUrl(page.url.clone()));
        }
    }
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
    Ok(local
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
                            declaration_source: "model_page".into(),
                            declaration_version: 1,
                        })
                })
        })
        .collect())
}
/// Reports observed links whose retained target declares a different canonical.
/// A page that names its own URL is a retained declaration, but a self-reference
/// is not a variant link and contributes no entry here. Duplicate local pages or
/// duplicate non-self declaration pages require caller reconciliation.
pub fn saved_canonical_variant_links(
    local: &[LocalPageEvidence],
    declarations: &[CanonicalDeclaration],
) -> Result<Vec<CanonicalVariantLink>, DuplicatePageUrl> {
    unique_pages(local)?;
    let mut by_page = BTreeMap::new();
    for declaration in declarations {
        if declaration.declared_canonical == declaration.page {
            continue;
        }
        if by_page
            .insert(declaration.page.as_str(), declaration)
            .is_some()
        {
            return Err(DuplicatePageUrl(declaration.page.clone()));
        }
    }
    Ok(local
        .iter()
        .flat_map(|source| {
            source
                .links
                .iter()
                .enumerate()
                .filter_map(|(ordinal, link)| {
                    by_page
                        .get(link.fetch_url.as_str())
                        .map(|declaration| CanonicalVariantLink {
                            source: source.url.clone(),
                            link_ordinal: ordinal,
                            target: link.fetch_url.clone(),
                            declared_canonical: declaration.declared_canonical.clone(),
                            declaration_source: declaration.declaration_source.clone(),
                            declaration_version: declaration.declaration_version,
                        })
                })
        })
        .collect())
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
