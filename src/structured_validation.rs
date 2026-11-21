//! Experimental source-only structured data checks. This module is not wired into reports.
use crate::model::{Page, StructuredData};
use serde_json::Value;
use std::collections::HashMap;
use url::Url;

pub const PROFILE_VERSION: &str = "google-subset-2026-11-21.1";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Observation {
    pub category: &'static str,
    pub code: &'static str,
    /// RFC 6901 pointer relative to the page's `structured_data` array.
    pub path: String,
    pub value: Option<Value>,
    pub explanation: String,
}

fn add(
    out: &mut Vec<Observation>,
    category: &'static str,
    code: &'static str,
    path: String,
    value: Option<&Value>,
    explanation: &str,
) {
    out.push(Observation {
        category,
        code,
        path,
        value: value.cloned(),
        explanation: explanation.into(),
    });
}
fn escape(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}
fn values(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        _ => vec![v],
    }
}
fn name(s: &str) -> &str {
    s.strip_prefix("https://schema.org/")
        .or_else(|| s.strip_prefix("http://schema.org/"))
        .unwrap_or(s)
}
fn types(v: &Value) -> Vec<&str> {
    v.get("@type")
        .or_else(|| v.get("types"))
        .map(values)
        .unwrap_or_default()
        .into_iter()
        .filter_map(Value::as_str)
        .map(name)
        .collect()
}
fn props(v: &Value) -> &Value {
    v.get("properties").unwrap_or(v)
}
fn collect_ids<'a>(v: &'a Value, base: &Url, ids: &mut HashMap<String, &'a Value>) {
    match v {
        Value::Object(m) => {
            if let Some(id) = m
                .get("@id")
                .or_else(|| m.get("id"))
                .and_then(Value::as_str)
                .filter(|_| !is_reference(v))
            {
                if let Ok(u) = base.join(id) {
                    ids.insert(u.to_string(), v);
                }
            }
            for child in m.values() {
                collect_ids(child, base, ids);
            }
        }
        Value::Array(a) => {
            for child in a {
                collect_ids(child, base, ids);
            }
        }
        _ => {}
    }
}
fn is_reference(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key("@id"))
}
fn present(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => a.iter().any(present),
        _ => true,
    }
}
fn check_item(
    v: &Value,
    path: &str,
    base: &Url,
    ids: &HashMap<String, &Value>,
    out: &mut Vec<Observation>,
) {
    let ts = types(v);
    let p = props(v);
    let prefix = if v.get("properties").is_some() {
        format!("{path}/properties")
    } else {
        path.to_string()
    };
    for t in ts {
        let (required, recommended): (&[&str], &[&str]) = match t {
            "Product" => (&["name"], &["image", "description"]),
            "Article" | "NewsArticle" | "BlogPosting" => (
                &[],
                &[
                    "headline",
                    "image",
                    "datePublished",
                    "dateModified",
                    "author",
                ],
            ),
            "BreadcrumbList" => (&["itemListElement"], &[]),
            "LocalBusiness" => (&["name", "address"], &["url", "telephone"]),
            _ => continue,
        };
        for (fields, code) in [
            (required, "missing_required"),
            (recommended, "missing_recommended"),
        ] {
            for field in fields {
                if !p.get(*field).is_some_and(present) {
                    add(
                        out,
                        "provider_eligibility",
                        code,
                        format!("{prefix}/{}", escape(field)),
                        None,
                        &format!("{t}: {field} is absent from this limited profile"),
                    );
                }
            }
        }
        if t == "Product"
            && !["offers", "review", "aggregateRating"]
                .iter()
                .any(|x| p.get(*x).is_some_and(present))
        {
            add(
                out,
                "provider_eligibility",
                "missing_product_evidence",
                path.into(),
                None,
                "Product snippet requires at least one of offers, review or aggregateRating",
            );
        }
        for field in ["name", "headline", "description", "telephone"] {
            if let Some(val) = p.get(field) {
                for x in values(val) {
                    if !x.is_string() && !is_reference(x) {
                        add(
                            out,
                            "vocabulary",
                            "unexpected_value_type",
                            format!("{prefix}/{}", escape(field)),
                            Some(x),
                            "Expected text or a reference; full vocabulary validation is unavailable",
                        );
                    }
                }
            }
        }
        if t == "BreadcrumbList" {
            if let Some(elements) = p.get("itemListElement") {
                for (i, item) in values(elements).iter().enumerate() {
                    let item_path = format!(
                        "{prefix}/itemListElement{}",
                        if elements.is_array() {
                            format!("/{i}")
                        } else {
                            String::new()
                        }
                    );
                    if item.is_object() && !is_reference(item) {
                        for field in ["position", "name"] {
                            if !props(item).get(field).is_some_and(present)
                                && !(field == "name"
                                    && props(item)
                                        .get("item")
                                        .and_then(|v| v.get("name"))
                                        .is_some_and(present))
                            {
                                add(
                                    out,
                                    "provider_eligibility",
                                    "missing_breadcrumb_property",
                                    format!("{item_path}/{field}"),
                                    None,
                                    "Breadcrumb ListItem property is absent; reference and context resolution are limited",
                                );
                            }
                        }
                    } else if !item.is_object() {
                        add(
                            out,
                            "vocabulary",
                            "unexpected_value_type",
                            item_path,
                            Some(item),
                            "Expected a ListItem or reference",
                        );
                    }
                }
            }
        }
        // Visibility cannot be established from source observations alone.
        add(
            out,
            "human_review",
            "visible_content_unverified",
            path.into(),
            None,
            "Compare markup with visible content and business facts; rendered visibility is unavailable",
        );
    }
    match v {
        Value::Object(m) => {
            if is_reference(v) {
                let id = v["@id"].as_str().unwrap_or_default();
                if let Ok(u) = base.join(id) {
                    if u.origin() == base.origin()
                        && u.path() == base.path()
                        && u.query() == base.query()
                        && !ids.contains_key(u.as_str())
                    {
                        add(
                            out,
                            "vocabulary",
                            "unresolved_local_id",
                            format!("{path}/@id"),
                            v.get("@id"),
                            "No matching local identifier was observed; extraction may be incomplete",
                        );
                    }
                }
            }
            for (k, child) in m {
                check_item(child, &format!("{path}/{}", escape(k)), base, ids, out);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                check_item(child, &format!("{path}/{i}"), base, ids, out);
            }
        }
        _ => {}
    }
}
/// Validate retained source observations only. No network requests or report mutation occur.
pub fn validate(page: &Page, observations: &[StructuredData]) -> Vec<Observation> {
    let mut out = Vec::new();
    let Ok(base) = Url::parse(&page.url) else {
        return out;
    };
    let mut ids = HashMap::new();
    for block in observations {
        if let Some(v) = &block.data {
            collect_ids(v, &base, &mut ids);
        }
    }
    for (i, block) in observations.iter().enumerate() {
        let path = format!("/{i}");
        if let Some(error) = &block.error {
            add(
                &mut out,
                "syntax",
                "invalid_source",
                format!("{path}/error"),
                None,
                error,
            );
        }
        if let Some(v) = &block.data {
            if block.format == "json_ld" {
                add(
                    &mut out,
                    "coverage",
                    "context_not_expanded",
                    format!("{path}/data"),
                    None,
                    "JSON-LD contexts and aliases are not expanded; vocabulary and eligibility conclusions are incomplete",
                );
            }
            check_item(v, &format!("{path}/data"), &base, &ids, &mut out);
        }
    }
    // Headings are a partial source proxy, not proof of rendered visibility.
    let headings = page
        .headings
        .iter()
        .map(|(_, s)| s.to_lowercase())
        .collect::<Vec<_>>();
    if !headings.is_empty() {
        for (i, block) in observations.iter().enumerate() {
            if let Some(data) = &block.data {
                heading_candidates(data, &format!("/{i}/data"), &headings, &mut out);
            }
        }
    }
    if page.extraction_truncated {
        add(
            &mut out,
            "coverage",
            "extraction_truncated",
            "".into(),
            None,
            "Source observations were truncated; absence and identifier checks are incomplete",
        );
    }
    out
}

/// Look up an explicitly referenced HTTP resource in already collected observations.
/// A missing, blocked, or failed observation remains unknown and never triggers a fetch.
pub fn referenced_resource(
    page_url: &str,
    reference: &str,
    pages: &[Page],
) -> (&'static str, Option<u16>) {
    let Some(url) = Url::parse(page_url)
        .ok()
        .and_then(|base| base.join(reference).ok())
    else {
        return ("unknown", None);
    };
    if !matches!(url.scheme(), "http" | "https") {
        return ("unknown", None);
    }
    let Some(found) = pages.iter().find(|p| p.url == url.as_str()) else {
        return ("not_observed", None);
    };
    if found.blocked_by_robots || found.error.is_some() {
        return ("unknown", found.status);
    }
    match found.status {
        Some(status) => ("observed", Some(status)),
        None => ("unknown", None),
    }
}

fn heading_candidates(v: &Value, path: &str, headings: &[String], out: &mut Vec<Observation>) {
    if let Some(headline) = props(v).get("headline").and_then(Value::as_str) {
        let h = headline.trim().to_lowercase();
        if !h.is_empty()
            && !headings
                .iter()
                .any(|visible| visible.contains(&h) || h.contains(visible))
        {
            add(
                out,
                "human_review",
                "headline_heading_mismatch_candidate",
                format!("{path}/headline"),
                Some(&Value::String(headline.into())),
                "Headline differs from retained source headings; this is not proof of hidden or mismatched content",
            );
        }
    }
    match v {
        Value::Object(m) => {
            for (key, child) in m {
                heading_candidates(child, &format!("{path}/{}", escape(key)), headings, out);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                heading_candidates(child, &format!("{path}/{i}"), headings, out);
            }
        }
        _ => {}
    }
}

/// Validate a page against the existing crawl inventory; no new network requests are made.
pub fn validate_crawl(page: &Page, pages: &[Page]) -> Vec<Observation> {
    let mut out = validate(page, &page.structured_data);
    for (i, block) in page.structured_data.iter().enumerate() {
        if let Some(v) = &block.data {
            resource_observations(v, &format!("/{i}/data"), page, pages, &mut out);
        }
    }
    out
}
fn resource_observations(
    v: &Value,
    path: &str,
    page: &Page,
    pages: &[Page],
    out: &mut Vec<Observation>,
) {
    match v {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}/{}", escape(key));
                if matches!(key.as_str(), "image" | "contentUrl" | "thumbnailUrl") {
                    for (i, item) in values(child).iter().enumerate() {
                        if let Some(reference) = item.as_str() {
                            let (state, status) = referenced_resource(&page.url, reference, pages);
                            let pointer = if child.is_array() {
                                format!("{child_path}/{i}")
                            } else {
                                child_path.clone()
                            };
                            add(
                                out,
                                "resource",
                                "referenced_resource_status",
                                pointer,
                                Some(item),
                                &format!(
                                    "state={state}; status={}",
                                    status.map_or_else(|| "unknown".into(), |s| s.to_string())
                                ),
                            );
                        }
                    }
                }
                resource_observations(child, &child_path, page, pages, out);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                resource_observations(child, &format!("{path}/{i}"), page, pages, out);
            }
        }
        _ => {}
    }
}
