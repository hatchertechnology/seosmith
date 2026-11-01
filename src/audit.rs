use crate::model::{Finding, Page, Severity, digest};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use url::Url;
#[derive(Clone, serde::Serialize)]
pub struct Rule {
    pub id: &'static str,
    pub severity: Severity,
    pub description: &'static str,
}
macro_rules! rule {
    ($id:literal, $sev:ident, $desc:literal) => {
        Rule {
            id: $id,
            severity: Severity::$sev,
            description: $desc,
        }
    };
}
#[must_use]
pub fn rules() -> Vec<Rule> {
    vec![
        rule!("invalid_language", Warning, "Invalid BCP47 language tag"),
        rule!("invalid_hreflang", Alert, "Invalid BCP47 hreflang tag"),
        rule!(
            "invalid_json_ld",
            Alert,
            "JSON-LD block is invalid JSON or has an invalid top-level value"
        ),
        rule!("meta_refresh", Warning, "HTML meta refresh directive"),
        rule!(
            "broken_resource",
            Alert,
            "Dependency has an observed HTTP error"
        ),
        rule!(
            "insecure_resource",
            Warning,
            "HTTPS page depends on HTTP resource"
        ),
        rule!(
            "encoding_errors",
            Warning,
            "Character decoding required replacements"
        ),
        rule!("http_redirect", Warning, "HTTP redirect"),
        rule!("http_client_error", Critical, "HTTP 4xx"),
        rule!("http_server_error", Critical, "HTTP 5xx"),
        rule!("fetch_error", Critical, "Fetch failed"),
        rule!(
            "external_link_broken",
            Alert,
            "External link has observed HTTP error"
        ),
        rule!("external_link_redirect", Warning, "External link redirects"),
        rule!("large_image", Warning, "Image exceeds 100 KiB"),
        rule!(
            "incorrect_media_type",
            Alert,
            "Media type conflicts with common file extension"
        ),
        rule!("robots_blocked", Warning, "Robots disallows fetching"),
        rule!("missing_title", Alert, "Missing title"),
        rule!("short_title", Warning, "Title shorter than 15 characters"),
        rule!("long_title", Warning, "Title longer than 60 characters"),
        rule!("missing_description", Alert, "Missing description"),
        rule!(
            "short_description",
            Warning,
            "Description shorter than 50 characters"
        ),
        rule!(
            "long_description",
            Warning,
            "Description longer than 160 characters"
        ),
        rule!("multiple_title_tags", Alert, "Multiple titles"),
        rule!("multiple_description_tags", Alert, "Multiple descriptions"),
        rule!("noindex", Warning, "Noindex directive"),
        rule!("nosnippet", Warning, "Nosnippet directive"),
        rule!("noimageindex", Warning, "Noimageindex directive"),
        rule!("missing_h1", Alert, "Missing nonempty H1"),
        rule!("heading_order", Warning, "Heading levels skip a level"),
        rule!("missing_language", Warning, "Missing document language"),
        rule!("little_content", Warning, "Fewer than 100 body words"),
        rule!("missing_image_alt", Alert, "Image without alt attribute"),
        rule!(
            "long_image_alt",
            Warning,
            "Image alt longer than 125 characters"
        ),
        rule!("image_without_size", Warning, "Image lacks width or height"),
        rule!("picture_without_img", Alert, "Picture without img"),
        rule!("duplicate_ids", Alert, "Duplicate HTML ID"),
        rule!("large_dom", Warning, "More than 1500 DOM elements"),
        rule!("missing_viewport", Alert, "Missing viewport"),
        rule!("meta_in_body", Warning, "Meta element in body"),
        rule!("form_on_http", Critical, "Form served over HTTP"),
        rule!("insecure_form", Critical, "Form submits over HTTP"),
        rule!("missing_hsts", Warning, "HTTPS response lacks HSTS"),
        rule!("missing_csp", Warning, "Missing CSP"),
        rule!("missing_nosniff", Warning, "Missing nosniff"),
        rule!("http_scheme", Warning, "HTTP URL"),
        rule!("http_links", Warning, "HTTPS page links to HTTP"),
        rule!("localhost_links", Alert, "Link targets localhost"),
        rule!("underscore_url", Warning, "Underscore in URL path"),
        rule!("space_url", Warning, "Encoded space in URL"),
        rule!("multiple_slashes", Warning, "Repeated path slash"),
        rule!("slow_ttfb", Warning, "Response headers took over 800ms"),
        rule!("deep_page", Warning, "Page depth exceeds 4"),
        rule!("too_many_links", Warning, "More than 200 outgoing links"),
        rule!("deadend", Warning, "No outgoing navigational links"),
        rule!("internal_nofollow", Warning, "Nofollow internal link"),
        rule!("multiple_canonicals", Alert, "Multiple canonical tags"),
        rule!("relative_canonical", Warning, "Relative canonical"),
        rule!(
            "canonical_mismatch",
            Alert,
            "HTTP and HTML canonicals differ"
        ),
        rule!("sitemap_noindex", Alert, "Sitemap includes noindex page"),
        rule!(
            "sitemap_blocked",
            Alert,
            "Sitemap includes robots-blocked URL"
        ),
        rule!(
            "sitemap_noncanonical",
            Alert,
            "Sitemap includes noncanonical URL"
        ),
        rule!("duplicate_title", Alert, "Pages share a title"),
        rule!("duplicate_description", Alert, "Pages share a description"),
        rule!(
            "duplicate_content",
            Alert,
            "Pages share normalized body text"
        ),
        rule!(
            "broken_internal_link",
            Critical,
            "Internal link has observed 4xx/5xx target"
        ),
        rule!(
            "redirect_chain",
            Alert,
            "Redirect points to another redirect"
        ),
        rule!("redirect_loop", Critical, "Observed redirect cycle"),
        rule!(
            "orphan_page",
            Alert,
            "Sitemap page has no observed incoming navigational links"
        ),
        rule!(
            "mixed_incoming_rel",
            Warning,
            "Incoming links mix follow and nofollow"
        ),
        rule!(
            "nofollow_indexable",
            Warning,
            "Nofollow link targets observed indexable page"
        ),
        rule!(
            "canonical_to_error",
            Alert,
            "Canonical target has HTTP error"
        ),
        rule!("canonical_to_redirect", Alert, "Canonical target redirects"),
        rule!("canonical_to_noindex", Alert, "Canonical target is noindex"),
        rule!(
            "canonical_chain",
            Alert,
            "Canonical target points elsewhere"
        ),
        rule!(
            "missing_hreflang_xdefault",
            Warning,
            "Hreflang lacks x-default"
        ),
        rule!(
            "missing_hreflang_self",
            Alert,
            "Hreflang lacks self-reference"
        ),
        rule!(
            "hreflang_language_mismatch",
            Alert,
            "Self-reference language differs from document"
        ),
        rule!("relative_hreflang", Warning, "Relative hreflang"),
        rule!(
            "hreflang_return_missing",
            Alert,
            "Observed alternate lacks return reference"
        ),
        rule!("hreflang_to_error", Alert, "Alternate has HTTP error"),
        rule!("hreflang_to_redirect", Alert, "Alternate redirects"),
        rule!("hreflang_to_noindex", Alert, "Alternate is noindex"),
        rule!("hreflang_noncanonical", Alert, "Alternate is noncanonical"),
        rule!(
            "hreflang_multiple_languages",
            Alert,
            "Same alternate has multiple language labels"
        ),
        rule!(
            "pagination_not_linked",
            Alert,
            "Pagination target has no navigational link"
        ),
    ]
}
fn add(out: &mut Vec<Finding>, p: &Page, rule: &Rule, evidence: String) {
    let key = digest(format!("{}\n{}", rule.id, p.url).as_bytes());
    out.push(Finding {
        key,
        rule: rule.id.into(),
        severity: rule.severity.clone(),
        url: p.url.clone(),
        evidence_items: vec![evidence.clone()],
        details: serde_json::json!({}),
        evidence,
    });
}
#[must_use]
pub fn analyze(pages: &[Page], seed: &Url) -> Vec<Finding> {
    let defs = rules();
    let by_url: HashMap<_, _> = pages.iter().map(|p| (p.url.as_str(), p)).collect();
    let mut groups: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut incoming: HashMap<&str, (usize, usize)> = HashMap::new();
    for p in pages {
        if p.html() && !p.is_resource {
            for (kind, value) in [
                ("duplicate_title", p.title.as_str()),
                ("duplicate_description", &p.description),
                ("duplicate_content", &p.body_hash),
            ] {
                if !value.is_empty() && (kind != "duplicate_content" || p.words > 0) {
                    *groups.entry((kind, value)).or_default() += 1;
                }
            }
        }
        for link in p.links.iter().filter(|_| !p.is_resource) {
            let count = incoming.entry(&link.url).or_default();
            if link.nofollow {
                count.1 += 1;
            } else {
                count.0 += 1;
            }
        }
    }
    let mut out = Vec::new();
    for p in pages {
        let mut found: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        let mut mark = |rule, evidence: String| {
            let values = found.entry(rule).or_default();
            if !values.contains(&evidence) && values.len() < 2000 {
                values.push(evidence);
            }
        };
        if p.blocked_by_robots {
            mark("robots_blocked", "Not fetched: robots.txt".into());
            if p.in_sitemap {
                mark("sitemap_blocked", "URL listed in sitemap".into());
            }
        }
        if let Some(err) = &p.error {
            mark("fetch_error", err.clone());
        }
        if let Some(code) = p.status {
            if (300..400).contains(&code) {
                mark("http_redirect", code.to_string());
            }
            if (400..500).contains(&code) {
                mark("http_client_error", code.to_string());
            }
            if p.content_type.starts_with("image/") && p.bytes > 102_400 {
                mark("large_image", format!("{} bytes", p.bytes));
            }
            if let Ok(url) = Url::parse(&p.url) {
                let path = url.path().to_ascii_lowercase();
                let expected = [
                    (".css", "text/css"),
                    (".js", "javascript"),
                    (".jpg", "image/jpeg"),
                    (".jpeg", "image/jpeg"),
                    (".png", "image/png"),
                    (".webp", "image/webp"),
                    (".svg", "image/svg+xml"),
                ];
                if code == 200
                    && expected.iter().any(|(extension, mime)| {
                        path.ends_with(extension) && !p.content_type.contains(mime)
                    })
                {
                    mark("incorrect_media_type", p.content_type.clone());
                }
            }
            if code >= 500 {
                mark("http_server_error", code.to_string());
            }
        }
        if let Ok(u) = Url::parse(&p.url) {
            if u.scheme() == "http" {
                mark("http_scheme", p.url.clone());
            }
            if u.path().contains('_') {
                mark("underscore_url", u.path().into());
            }
            if u.path().contains("%20") {
                mark("space_url", u.path().into());
            }
            if u.path().contains("//") {
                mark("multiple_slashes", u.path().into());
            }
        }
        if p.ttfb_ms > 800 {
            mark("slow_ttfb", format!("{}ms", p.ttfb_ms));
        }
        if p.depth > 4 {
            mark("deep_page", p.depth.to_string());
        }
        for relationship in &p.relationships {
            if relationship.resource {
                if p.url.starts_with("https:") && relationship.url.starts_with("http:") {
                    mark("insecure_resource", relationship.url.clone());
                }
                if by_url
                    .get(relationship.url.as_str())
                    .is_some_and(|t| t.status.is_some_and(|s| s >= 400))
                {
                    mark("broken_resource", relationship.url.clone());
                }
            }
        }
        if p.html() && !p.is_resource {
            if !p.language.is_empty() && !valid_language(&p.language) {
                mark("invalid_language", p.language.clone());
            }
            for alternate in &p.alternates {
                if !alternate.language.eq_ignore_ascii_case("x-default")
                    && !valid_language(&alternate.language)
                {
                    mark("invalid_hreflang", alternate.language.clone());
                }
            }
            for (index, block) in p.structured_data.iter().enumerate() {
                if block.format == "json_ld"
                    && let Some(error) =
                        block.error.as_ref().filter(|e| e.as_str() != "block_limit")
                {
                    mark("invalid_json_ld", format!("block {index}: {error}"));
                }
            }
            if let Some(delay) = p.refresh_delay {
                mark(
                    "meta_refresh",
                    format!(
                        "{delay}s: {}",
                        p.refresh_target.as_deref().unwrap_or("reload")
                    ),
                );
            }
            for flag in &p.flags {
                if let Some(def) = defs.iter().find(|r| r.id == flag) {
                    mark(def.id, flag.clone());
                }
            }
            for (value, missing, short, long, min, max) in [
                (
                    &p.title,
                    "missing_title",
                    "short_title",
                    "long_title",
                    15,
                    60,
                ),
                (
                    &p.description,
                    "missing_description",
                    "short_description",
                    "long_description",
                    50,
                    160,
                ),
            ] {
                let n = value.chars().count();
                if n == 0 {
                    mark(missing, "Empty".into());
                } else if n < min {
                    mark(short, format!("{n} characters"));
                } else if n > max {
                    mark(long, format!("{n} characters"));
                }
            }
            if p.language.is_empty() {
                mark("missing_language", "Empty lang attribute".into());
            }
            if !p.headings.iter().any(|(n, t)| *n == 1 && !t.is_empty()) {
                mark("missing_h1", "No nonempty H1".into());
            }
            if p.headings.windows(2).any(|w| w[1].0 > w[0].0 + 1) {
                mark("heading_order", "Skipped heading level".into());
            }
            if p.words < 100 {
                mark("little_content", format!("{} words", p.words));
            }
            if p.dom_nodes > 1500 {
                mark("large_dom", p.dom_nodes.to_string());
            }
            if p.images.iter().any(|i| i.alt.is_none()) {
                mark(
                    "missing_image_alt",
                    "Missing alt; empty decorative alt is allowed".into(),
                );
            }
            if p.images
                .iter()
                .any(|i| i.alt.as_ref().is_some_and(|a| a.chars().count() > 125))
            {
                mark("long_image_alt", "Alt exceeds 125 characters".into());
            }
            if p.images.iter().any(|i| !i.width || !i.height) {
                mark("image_without_size", "Width or height missing".into());
            }
            if p.noindex() {
                mark("noindex", p.robots.clone());
                if p.in_sitemap {
                    mark("sitemap_noindex", p.robots.clone());
                }
            }
            for (directive, rule) in [("nosnippet", "nosnippet"), ("noimageindex", "noimageindex")]
            {
                if p.robots
                    .to_ascii_lowercase()
                    .split([',', ' ', ';'])
                    .any(|x| x == directive)
                {
                    mark(rule, p.robots.clone());
                }
            }
            if p.links.is_empty() {
                mark("deadend", "No navigational links".into());
            }
            if p.links.len() > 200 {
                mark("too_many_links", p.links.len().to_string());
            }
            for link in &p.links {
                if let Ok(u) = Url::parse(&link.url) {
                    if p.url.starts_with("https:") && u.scheme() == "http" {
                        mark("http_links", link.url.clone());
                    }
                    if matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) {
                        mark("localhost_links", link.url.clone());
                    }
                    if u.host_str() == seed.host_str() && link.nofollow {
                        mark("internal_nofollow", link.url.clone());
                    }
                }
                if let Some(target) = by_url.get(link.url.as_str()) {
                    let external =
                        Url::parse(&link.url).is_ok_and(|u| u.host_str() != seed.host_str());
                    if target.status.is_some_and(|c| c >= 400) {
                        mark(
                            if external {
                                "external_link_broken"
                            } else {
                                "broken_internal_link"
                            },
                            link.url.clone(),
                        );
                    }
                    if external && target.status.is_some_and(|c| (300..400).contains(&c)) {
                        mark("external_link_redirect", link.url.clone());
                    }
                    if link.nofollow && target.html() && !target.noindex() {
                        mark("nofollow_indexable", link.url.clone());
                    }
                }
            }
            for (kind, value) in [
                ("duplicate_title", p.title.as_str()),
                ("duplicate_description", &p.description),
                ("duplicate_content", &p.body_hash),
            ] {
                if groups.get(&(kind, value)).is_some_and(|n| *n > 1) {
                    mark(kind, "Shared by multiple observed pages".into());
                }
            }
            if p.in_sitemap && p.url != seed.as_str() && !incoming.contains_key(p.url.as_str()) {
                mark(
                    "orphan_page",
                    "No incoming links within observed crawl".into(),
                );
            }
            if incoming
                .get(p.url.as_str())
                .is_some_and(|(a, b)| *a > 0 && *b > 0)
            {
                mark(
                    "mixed_incoming_rel",
                    "Both follow and nofollow incoming links".into(),
                );
            }
            if let Some(canonical) = &p.canonical {
                if p.in_sitemap && canonical != &p.url {
                    mark("sitemap_noncanonical", canonical.clone());
                }
                if p.header_canonical.as_ref().is_some_and(|h| h != canonical) {
                    mark("canonical_mismatch", canonical.clone());
                }
                if let Some(target) = by_url.get(canonical.as_str()) {
                    if target.status.is_some_and(|c| c >= 400) {
                        mark("canonical_to_error", canonical.clone());
                    }
                    if target.status.is_some_and(|c| (300..400).contains(&c)) {
                        mark("canonical_to_redirect", canonical.clone());
                    }
                    if target.noindex() {
                        mark("canonical_to_noindex", canonical.clone());
                    }
                    if target.canonical.as_ref().is_some_and(|c| c != canonical) {
                        mark("canonical_chain", canonical.clone());
                    }
                }
            }
            if !p.alternates.is_empty() {
                if !p.alternates.iter().any(|a| a.language == "x-default") {
                    mark("missing_hreflang_xdefault", "No x-default alternate".into());
                }
                if !p.alternates.iter().any(|a| a.url == p.url) {
                    mark("missing_hreflang_self", "No self alternate".into());
                }
                let mut languages: HashMap<&str, BTreeSet<&str>> = HashMap::new();
                for a in &p.alternates {
                    languages.entry(&a.url).or_default().insert(&a.language);
                    if a.url == p.url
                        && a.language != "x-default"
                        && !a.language.eq_ignore_ascii_case(&p.language)
                    {
                        mark("hreflang_language_mismatch", a.language.clone());
                    }
                    if let Some(target) = by_url.get(a.url.as_str()) {
                        if target.status.is_some_and(|c| c >= 400) {
                            mark("hreflang_to_error", a.url.clone());
                        }
                        if target.status.is_some_and(|c| (300..400).contains(&c)) {
                            mark("hreflang_to_redirect", a.url.clone());
                        }
                        if target.noindex() {
                            mark("hreflang_to_noindex", a.url.clone());
                        }
                        if target.canonical.as_ref().is_some_and(|c| c != &target.url) {
                            mark("hreflang_noncanonical", a.url.clone());
                        }
                        if target.html() && !target.alternates.iter().any(|alt| alt.url == p.url) {
                            mark("hreflang_return_missing", a.url.clone());
                        }
                    }
                }
                if languages
                    .values()
                    .any(|langs| langs.iter().filter(|l| **l != "x-default").count() > 1)
                {
                    mark(
                        "hreflang_multiple_languages",
                        "Alternate has multiple language labels".into(),
                    );
                }
            }
        }
        if let Some(target) = p.redirect_target() {
            if by_url
                .get(target)
                .is_some_and(|t| t.redirect_target().is_some())
            {
                mark("redirect_chain", target.into());
            }
            let mut visited = HashSet::new();
            visited.insert(p.url.as_str());
            let mut next = Some(target);
            while let Some(u) = next {
                if !visited.insert(u) {
                    mark("redirect_loop", u.into());
                    break;
                }
                next = by_url.get(u).and_then(|t| t.redirect_target());
            }
        }
        for (id, items) in found {
            if let Some(def) = defs.iter().find(|r| r.id == id) {
                add(&mut out, p, def, items.first().cloned().unwrap_or_default());
                if let Some(finding) = out.last_mut() {
                    finding.evidence_items = items;
                    finding.details = serde_json::json!({"status":p.status,"bytes":p.bytes,"ttfb_ms":p.ttfb_ms,"words":p.words,"dom_nodes":p.dom_nodes,"observation_count":finding.evidence_items.len()});
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.url, &a.rule).cmp(&(&b.url, &b.rule)));
    out
}

fn valid_language(value: &str) -> bool {
    language_tags::LanguageTag::parse(value).is_ok_and(|tag| tag.validate().is_ok())
}
