use crate::{
    fetch::Response,
    model::{Alternate, Image, Link, Page, digest, normalize},
};
use scraper::{Html, Selector};
use std::collections::HashSet;
use url::Url;
fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static CSS selector")
}
fn text(el: scraper::ElementRef<'_>) -> String {
    el.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
#[must_use]
pub fn parse(url: &Url, depth: usize, in_sitemap: bool, response: &Response) -> Page {
    let mut p = Page {
        url: url.to_string(),
        depth,
        in_sitemap,
        status: Some(response.status),
        bytes: response.body.len(),
        ttfb_ms: response.ttfb_ms,
        content_type: response
            .headers
            .get("content-type")
            .cloned()
            .unwrap_or_default(),
        robots: response
            .headers
            .get("x-robots-tag")
            .cloned()
            .unwrap_or_default(),
        redirect: response
            .headers
            .get("location")
            .and_then(|v| normalize(url, v))
            .map(Into::into),

        ..Page::default()
    };
    if let Some(value) = response.headers.get("link") {
        crate::extract::headers(&mut p, url, value);
    }
    let is_css = p
        .content_type
        .split(';')
        .next()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("text/css"));
    if !p.html() && !is_css {
        p.canonical.clone_from(&p.header_canonical);
        return p;
    }
    let (decoded, encoding, errors) = crate::extract::decode(&response.body, &p.content_type);
    p.encoding = encoding;
    if errors {
        p.flags.push("encoding_errors".into());
    }
    if !p.html() {
        if is_css {
            let (targets, truncated) = crate::extract::css_observations(&decoded);
            p.extraction_truncated |= truncated;
            for target in targets {
                crate::extract::relationship(&mut p, url, "css_url", &target, "", true);
            }
        }
        if p.extraction_truncated {
            p.flags.push("extraction_limit".into());
        }
        p.canonical.clone_from(&p.header_canonical);
        return p;
    }
    let html = Html::parse_document(&decoded);
    let base = html
        .select(&sel("base[href]"))
        .next()
        .and_then(|x| normalize(url, x.value().attr("href")?))
        .unwrap_or_else(|| url.clone());
    let mut flags: Vec<&str> = if errors {
        vec!["encoding_errors"]
    } else {
        Vec::new()
    };
    let titles: Vec<_> = html.select(&sel("head title")).collect();
    p.title = titles.first().copied().map_or_else(String::new, text);
    if titles.len() > 1 {
        flags.push("multiple_title_tags");
    }
    let mut descriptions = Vec::new();
    let mut canonicals = Vec::new();
    let mut viewport = false;
    p.language = html
        .select(&sel("html"))
        .next()
        .and_then(|x| x.value().attr("lang"))
        .unwrap_or_default()
        .into();
    p.language_source = if p.language.is_empty() {
        String::new()
    } else {
        "html".into()
    };
    if p.language.is_empty()
        && let Some(value) = response.headers.get("content-language")
    {
        p.language = value.split(',').next().unwrap_or_default().trim().into();
        p.language_source = "http_header".into();
    }
    for meta in html.select(&sel("head meta")) {
        let name = meta
            .value()
            .attr("name")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let content = meta.value().attr("content").unwrap_or_default();
        match name.as_str() {
            "description" => descriptions.push(content.to_owned()),
            "robots" | "seosmith" => {
                p.robots.push(',');
                p.robots.push_str(content);
            }
            "viewport" => viewport = true,
            _ => {}
        }
    }
    p.description = descriptions.first().cloned().unwrap_or_default();
    if descriptions.len() > 1 {
        flags.push("multiple_description_tags");
    }
    if !viewport {
        flags.push("missing_viewport");
    }
    if html.select(&sel("body meta")).next().is_some() {
        flags.push("meta_in_body");
    }
    let mut pagination = Vec::new();
    for link in html.select(&sel("link[href]")).take(2000) {
        let rel = link
            .value()
            .attr("rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let raw = link.value().attr("href").unwrap_or_default();
        let Some(target) = normalize(&base, raw) else {
            continue;
        };
        if rel.split_whitespace().any(|r| r == "canonical") {
            if Url::parse(raw).is_err() {
                flags.push("relative_canonical");
            }
            canonicals.push(target.to_string());
        }
        if let Some(lang) = link.value().attr("hreflang") {
            if Url::parse(raw).is_err() {
                flags.push("relative_hreflang");
            }
            p.alternates.push(Alternate {
                language: lang.into(),
                url: target.to_string(),
            });
        }
        if rel == "next" || rel == "prev" {
            pagination.push(target.to_string());
        }
    }
    if canonicals.len() > 1 {
        flags.push("multiple_canonicals");
    }
    p.canonical = canonicals
        .first()
        .cloned()
        .or_else(|| p.header_canonical.clone());
    for el in html.select(&sel("a[href], area[href]")).take(2000) {
        if let Some(target) = el.value().attr("href").and_then(|s| normalize(&base, s)) {
            let rel = el
                .value()
                .attr("rel")
                .unwrap_or_default()
                .to_ascii_lowercase();
            p.links.push(Link {
                url: target.to_string(),
                nofollow: rel.split_whitespace().any(|x| x == "nofollow"),
                sponsored: rel.split_whitespace().any(|x| x == "sponsored"),
                ugc: rel.split_whitespace().any(|x| x == "ugc"),
                rel: rel.split_whitespace().map(str::to_owned).collect(),
                text: text(el),
            });
        }
    }
    if pagination
        .iter()
        .any(|u| !p.links.iter().any(|l| &l.url == u))
    {
        flags.push("pagination_not_linked");
    }
    for el in html.select(&sel("img")).take(2000) {
        p.images.push(Image {
            url: el
                .value()
                .attr("src")
                .and_then(|s| normalize(&base, s))
                .map_or_else(String::new, Into::into),
            alt: el.value().attr("alt").map(str::to_owned),
            width: el.value().attr("width").is_some(),
            height: el.value().attr("height").is_some(),
        });
    }
    if html
        .select(&sel("picture"))
        .any(|x| x.select(&sel("img")).next().is_none())
    {
        flags.push("picture_without_img");
    }
    for el in html.select(&sel("h1,h2,h3,h4,h5,h6")) {
        p.headings
            .push((el.value().name().as_bytes()[1] - b'0', text(el)));
    }
    let mut ids = HashSet::new();
    if html
        .select(&sel("[id]"))
        .any(|x| !ids.insert(x.value().attr("id").unwrap_or_default().to_owned()))
    {
        flags.push("duplicate_ids");
    }
    for form in html.select(&sel("form")) {
        if url.scheme() == "http" {
            flags.push("form_on_http");
        }
        if form
            .value()
            .attr("action")
            .and_then(|s| normalize(&base, s))
            .is_some_and(|u| u.scheme() == "http")
        {
            flags.push("insecure_form");
        }
    }
    let body_text = html
        .select(&sel("body"))
        .next()
        .map_or_else(String::new, |body| {
            body.descendants()
                .filter_map(|n| {
                    let t = n.value().as_text()?;
                    let ignored = n
                        .ancestors()
                        .filter_map(scraper::ElementRef::wrap)
                        .any(|a| {
                            matches!(
                                a.value().name(),
                                "script" | "style" | "noscript" | "template"
                            )
                        });
                    (!ignored).then(|| t.to_string())
                })
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
    p.words = body_text.split_whitespace().count();
    p.body_hash = digest(body_text.as_bytes());
    p.dom_nodes = html.select(&sel("*")).count();
    if url.scheme() == "https" && !response.headers.contains_key("strict-transport-security") {
        flags.push("missing_hsts");
    }
    if !response.headers.contains_key("content-security-policy") {
        flags.push("missing_csp");
    }
    if !response
        .headers
        .get("x-content-type-options")
        .is_some_and(|v| v.eq_ignore_ascii_case("nosniff"))
    {
        flags.push("missing_nosniff");
    }
    if html.select(&sel("a[href],area[href]")).count() > 2000
        || html.select(&sel("img")).count() > 2000
        || html.select(&sel("link[href]")).count() > 2000
    {
        flags.push("extraction_limit");
    }
    p.title = p.title.chars().take(2048).collect();
    p.description = p.description.chars().take(2048).collect();
    for link in &mut p.links {
        link.text = link.text.chars().take(256).collect();
    }
    for heading in &mut p.headings {
        heading.1 = heading.1.chars().take(1024).collect();
    }
    crate::extract::enrich(&mut p, &html, &base);
    if let Some(target) = p.canonical.clone() {
        crate::extract::relationship(&mut p, &base, "canonical", &target, "", false);
    }
    for alternate in p.alternates.clone() {
        crate::extract::relationship(
            &mut p,
            &base,
            "hreflang",
            &alternate.url,
            &alternate.language,
            false,
        );
    }
    for link in p.links.clone() {
        crate::extract::relationship(
            &mut p,
            &base,
            "navigation",
            &link.url,
            &link.rel.join(" "),
            false,
        );
    }
    for image in p.images.clone() {
        crate::extract::relationship(&mut p, &base, "image", &image.url, "", true);
    }
    if let Some(target) = p.refresh_target.clone() {
        crate::extract::relationship(&mut p, &base, "meta_refresh", &target, "", false);
    }
    if p.extraction_truncated {
        flags.push("extraction_limit");
    }
    p.flags = flags.into_iter().map(str::to_owned).collect();
    p
}
