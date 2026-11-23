//! Experimental source-only structured data checks. This module is not wired into reports.
use crate::model::{Page, StructuredData};
use serde_json::Value;
use std::collections::HashMap;
use url::Url;

const MAX_OBSERVATIONS: usize = 512;
const MAX_VALUE_BYTES: usize = 4096;
// Reserve room for the collector envelope within the proposed 1 MiB page cap.
const MAX_OBSERVATION_BYTES: usize = 512 * 1024;

pub const PROFILE_VERSION: &str = "google-subset-2026-11-21.4";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Observation {
    pub category: &'static str,
    pub code: &'static str,
    /// RFC 6901 pointer relative to the page's `structured_data` array.
    pub path: String,
    pub value: Option<Value>,
    pub value_omitted: bool,
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
    if out.len() >= MAX_OBSERVATIONS {
        return;
    }
    if out.len() == MAX_OBSERVATIONS - 1 {
        out.push(Observation {
            category: "coverage",
            code: "observation_limit",
            path: String::new(),
            value: None,
            value_omitted: false,
            explanation:
                "Validation output reached the per-page limit; additional observations were omitted"
                    .into(),
        });
        return;
    }
    let retained = value
        .filter(|v| serde_json::to_vec(v).is_ok_and(|bytes| bytes.len() <= MAX_VALUE_BYTES))
        .cloned();
    out.push(Observation {
        category,
        code,
        path,
        value_omitted: value.is_some() && retained.is_none(),
        value: retained,
        explanation: explanation.into(),
    });
}
fn bound_bytes(out: &mut Vec<Observation>) {
    let mut bytes = 2usize; // JSON array brackets
    let mut keep = 0;
    // Reserve enough room for the explicit omission marker.
    let budget = MAX_OBSERVATION_BYTES - 512;
    for observation in out.iter() {
        let Ok(encoded) = serde_json::to_vec(observation) else {
            break;
        };
        let next = bytes.saturating_add(encoded.len()).saturating_add(1);
        if next > budget {
            break;
        }
        bytes = next;
        keep += 1;
    }
    if keep < out.len() {
        let omitted = out.len() - keep;
        out.truncate(keep);
        out.push(Observation { category: "coverage", code: "observation_byte_limit", path: String::new(), value: None, value_omitted: false, explanation: format!("Validation observations exceeded the per-page byte budget; {omitted} retained observations were omitted; prior count-limit omissions, if any, are unknown") });
    }
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
        .map_or_else(Vec::new, values)
        .into_iter()
        .filter_map(Value::as_str)
        .map(name)
        .collect()
}
fn props(v: &Value) -> &Value {
    v.get("properties").unwrap_or(v)
}
fn unsupported_context(v: &Value) -> bool {
    v.get("@context").is_some_and(|context| !matches!(context, Value::String(s) if matches!(s.as_str(), "https://schema.org" | "http://schema.org" | "https://schema.org/" | "http://schema.org/")))
}
type IdIndex<'a> = HashMap<String, Vec<(&'a Value, String)>>;
fn collect_ids<'a>(v: &'a Value, path: &str, base: &Url, ids: &mut IdIndex<'a>) {
    if unsupported_context(v) {
        return;
    }
    match v {
        Value::Object(m) => {
            if let Some(id) = m
                .get("@id")
                .or_else(|| m.get("id"))
                .and_then(Value::as_str)
                .filter(|_| !is_reference(v))
                && let Ok(u) = base.join(id)
            {
                ids.entry(u.to_string()).or_default().push((v, path.into()));
            }
            for (key, child) in m {
                collect_ids(child, &format!("{path}/{}", escape(key)), base, ids);
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                collect_ids(child, &format!("{path}/{i}"), base, ids);
            }
        }
        _ => {}
    }
}
fn unique_reference<'a>(
    v: &Value,
    base: Option<&Url>,
    ids: &'a IdIndex<'a>,
) -> Option<(&'a Value, &'a str)> {
    let base = base?;
    let id = v.get("@id")?.as_str()?;
    let target = base.join(id).ok()?;
    if target.origin() != base.origin()
        || target.path() != base.path()
        || target.query() != base.query()
    {
        return None;
    }
    let entries = ids.get(target.as_str())?;
    if entries.len() != 1 {
        return None;
    }
    Some((entries[0].0, entries[0].1.as_str()))
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
    base: Option<&Url>,
    ids: &IdIndex<'_>,
    out: &mut Vec<Observation>,
) {
    // Foreign and custom JSON-LD contexts may redefine every compact term.
    // Do not interpret that subtree as schema.org without expansion.
    if unsupported_context(v) {
        add(
            out,
            "coverage",
            "unsupported_context",
            format!("{path}/@context"),
            v.get("@context"),
            "Custom or remote context is not expanded; subtree checks are unavailable",
        );
        return;
    }
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
            "LocalBusiness" | "Restaurant" | "Store" | "Pharmacy" => {
                (&["name", "address"], &["url", "telephone"])
            }
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
                for (i, x) in values(val).iter().enumerate() {
                    if !x.is_string() && !is_reference(x) {
                        add(
                            out,
                            "vocabulary",
                            "unexpected_value_type",
                            format!(
                                "{prefix}/{}{}",
                                escape(field),
                                if val.is_array() {
                                    format!("/{i}")
                                } else {
                                    String::new()
                                }
                            ),
                            Some(x),
                            "Expected text or a reference; full vocabulary validation is unavailable",
                        );
                    }
                }
            }
        }
        if matches!(t, "Article" | "NewsArticle" | "BlogPosting")
            && let Some(authors) = p.get("author")
        {
            for (i, author) in values(authors).iter().enumerate() {
                let source_path = format!(
                    "{prefix}/author{}",
                    if authors.is_array() {
                        format!("/{i}")
                    } else {
                        String::new()
                    }
                );
                let resolved = if is_reference(author) {
                    unique_reference(author, base, ids)
                } else {
                    None
                };
                let (author, author_path) = if let Some((target, target_path)) = resolved {
                    add(
                        out,
                        "coverage",
                        "local_author_resolved",
                        source_path.clone(),
                        Some(author),
                        "Unique local author reference resolved to a retained definition",
                    );
                    (target, target_path.to_string())
                } else {
                    (*author, source_path)
                };
                if unsupported_context(author) {
                    continue;
                }
                if is_reference(author) {
                    add(
                        out,
                        "coverage",
                        "author_not_expanded",
                        author_path,
                        Some(author),
                        "Author reference could not be uniquely resolved",
                    );
                } else if author.is_object() {
                    let author_types = types(author);
                    if !author_types.is_empty()
                        && !author_types
                            .iter()
                            .any(|t| matches!(*t, "Person" | "Organization"))
                    {
                        add(
                            out,
                            "vocabulary",
                            "author_type_candidate",
                            author_path.clone(),
                            Some(author),
                            "Article author should be a Person or Organization; custom types may need review",
                        );
                    }
                    if !props(author).get("name").is_some_and(present) {
                        add(
                            out,
                            "provider_eligibility",
                            "missing_recommended_author_name",
                            format!(
                                "{author_path}{}/name",
                                if author.get("properties").is_some() {
                                    "/properties"
                                } else {
                                    ""
                                }
                            ),
                            None,
                            "Author name is recommended where applicable",
                        );
                    }
                } else {
                    add(
                        out,
                        "vocabulary",
                        "author_type_candidate",
                        author_path,
                        Some(author),
                        "Article author should be a Person or Organization",
                    );
                }
            }
        }
        if t == "Product"
            && let Some(offers) = p.get("offers")
        {
            for (i, offer) in values(offers).iter().enumerate() {
                let offer_path = format!(
                    "{prefix}/offers{}",
                    if offers.is_array() {
                        format!("/{i}")
                    } else {
                        String::new()
                    }
                );
                let resolved = if is_reference(offer) {
                    unique_reference(offer, base, ids)
                } else {
                    None
                };
                let (offer, offer_path) = if let Some((target, target_path)) = resolved {
                    add(
                        out,
                        "coverage",
                        "local_offer_resolved",
                        offer_path.clone(),
                        Some(offer),
                        "Unique local reference resolved to a retained definition; target property paths point to that definition",
                    );
                    (target, target_path.to_string())
                } else {
                    (*offer, offer_path)
                };
                if unsupported_context(offer) {
                    continue;
                }
                if is_reference(offer) {
                    add(
                        out,
                        "coverage",
                        "referenced_offer_not_expanded",
                        offer_path,
                        Some(offer),
                        "Offer reference requires graph expansion before its properties can be assessed",
                    );
                } else if offer.is_object() {
                    let op = props(offer);
                    let opath = if offer.get("properties").is_some() {
                        format!("{offer_path}/properties")
                    } else {
                        offer_path.clone()
                    };
                    for field in ["price", "lowPrice", "priceCurrency"] {
                        if let Some(raw) = op.get(field) {
                            for (j, value) in values(raw).iter().enumerate() {
                                let valid = if field == "priceCurrency" {
                                    value.as_str().is_some_and(|s| {
                                        s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase())
                                    })
                                } else {
                                    value.as_f64().is_some_and(|n| n >= 0.0)
                                        || value.as_str().is_some_and(|s| {
                                            s.parse::<f64>()
                                                .is_ok_and(|n| n.is_finite() && n >= 0.0)
                                        })
                                };
                                if !valid {
                                    add(
                                        out,
                                        "vocabulary",
                                        "invalid_offer_value",
                                        format!(
                                            "{opath}/{field}{}",
                                            if raw.is_array() {
                                                format!("/{j}")
                                            } else {
                                                String::new()
                                            }
                                        ),
                                        Some(value),
                                        "Expected a nonnegative numeric price or an uppercase three-letter currency code",
                                    );
                                }
                            }
                        }
                    }
                    let offer_types = types(offer);
                    if offer_types.contains(&"AggregateOffer") {
                        for field in ["lowPrice", "priceCurrency"] {
                            if !op.get(field).is_some_and(present) {
                                add(
                                    out,
                                    "provider_eligibility",
                                    "missing_aggregate_offer_property",
                                    format!("{opath}/{field}"),
                                    None,
                                    "AggregateOffer property is absent from the observed item",
                                );
                            }
                        }
                    } else if offer_types.contains(&"Offer") {
                        let mut has_price = op.get("price").is_some_and(present);
                        let mut unknown = false;
                        if let Some(specifications) = op.get("priceSpecification") {
                            for (i, specification) in values(specifications).iter().enumerate() {
                                let path = format!(
                                    "{opath}/priceSpecification{}",
                                    if specifications.is_array() {
                                        format!("/{i}")
                                    } else {
                                        String::new()
                                    }
                                );
                                let resolved = if is_reference(specification) {
                                    unique_reference(specification, base, ids)
                                } else {
                                    None
                                };
                                let (specification, target_path) = if let Some((
                                    target,
                                    target_path,
                                )) = resolved
                                {
                                    add(
                                        out,
                                        "coverage",
                                        "local_price_specification_resolved",
                                        path.clone(),
                                        Some(specification),
                                        "Unique local price specification resolved to a retained definition",
                                    );
                                    (target, target_path.to_string())
                                } else {
                                    (*specification, path)
                                };
                                if unsupported_context(specification) {
                                    unknown = true;
                                    continue;
                                }
                                if is_reference(specification) {
                                    unknown = true;
                                    add(
                                        out,
                                        "coverage",
                                        "price_specification_not_expanded",
                                        target_path,
                                        Some(specification),
                                        "Referenced price specification could not be uniquely resolved",
                                    );
                                } else if props(specification).get("price").is_some_and(present) {
                                    has_price = true;
                                }
                            }
                        }
                        if !op.get("priceCurrency").is_some_and(present) {
                            if op.get("priceSpecification").is_some_and(present) {
                                add(
                                    out,
                                    "coverage",
                                    "offer_currency_unknown",
                                    format!("{opath}/priceSpecification"),
                                    None,
                                    "Currency may be supplied by a price specification; full currency alternatives are not assessed",
                                );
                            } else {
                                add(
                                    out,
                                    "provider_eligibility",
                                    "missing_offer_currency",
                                    format!("{opath}/priceCurrency"),
                                    None,
                                    "Offer currency was not observed",
                                );
                            }
                        }
                        if !has_price && !unknown {
                            add(
                                out,
                                "provider_eligibility",
                                "missing_offer_price",
                                format!("{opath}/price"),
                                None,
                                "Offer price or priceSpecification.price was not observed",
                            );
                        }
                    } else {
                        add(
                            out,
                            "coverage",
                            "offer_type_unknown",
                            offer_path,
                            Some(offer),
                            "Offer type is not recognized; required properties were not assessed",
                        );
                    }
                } else {
                    add(
                        out,
                        "vocabulary",
                        "unexpected_value_type",
                        offer_path,
                        Some(offer),
                        "Expected an Offer, AggregateOffer or reference",
                    );
                }
            }
        }
        if matches!(t, "LocalBusiness" | "Restaurant" | "Store" | "Pharmacy")
            && let Some(address) = p.get("address")
        {
            for (i, entry) in values(address).iter().enumerate() {
                let location = format!(
                    "{prefix}/address{}",
                    if address.is_array() {
                        format!("/{i}")
                    } else {
                        String::new()
                    }
                );
                let resolved = if is_reference(entry) {
                    unique_reference(entry, base, ids)
                } else {
                    None
                };
                let (entry, location) = if let Some((target, target_path)) = resolved {
                    add(
                        out,
                        "coverage",
                        "local_address_resolved",
                        location.clone(),
                        Some(entry),
                        "Unique local address reference resolved to a retained definition",
                    );
                    (target, target_path.to_string())
                } else {
                    (*entry, location)
                };
                if unsupported_context(entry) {
                    continue;
                }
                if is_reference(entry) {
                    add(
                        out,
                        "coverage",
                        "address_not_expanded",
                        location,
                        Some(entry),
                        "Referenced address could not be uniquely resolved",
                    );
                } else if !entry.is_object() {
                    add(
                        out,
                        "vocabulary",
                        "address_type_candidate",
                        location,
                        Some(entry),
                        "LocalBusiness provider profile expects PostalAddress; context expansion is unavailable",
                    );
                }
            }
        }
        if t == "BreadcrumbList"
            && let Some(elements) = p.get("itemListElement")
        {
            for (i, item) in values(elements).iter().enumerate() {
                let item_path = format!(
                    "{prefix}/itemListElement{}",
                    if elements.is_array() {
                        format!("/{i}")
                    } else {
                        String::new()
                    }
                );
                if unsupported_context(item) {
                    continue;
                }
                if let Some(position) = props(item).get("position") {
                    for (j, value) in values(position).iter().enumerate() {
                        let valid = value.as_u64().is_some_and(|n| n > 0)
                            || value
                                .as_str()
                                .is_some_and(|s| s.parse::<u64>().is_ok_and(|n| n > 0));
                        if !valid {
                            let position_path = format!(
                                "{item_path}{}/position{}",
                                if item.get("properties").is_some() {
                                    "/properties"
                                } else {
                                    ""
                                },
                                if position.is_array() {
                                    format!("/{j}")
                                } else {
                                    String::new()
                                }
                            );
                            add(
                                out,
                                "vocabulary",
                                "invalid_breadcrumb_position",
                                position_path,
                                Some(value),
                                "Expected a positive integer position",
                            );
                        }
                    }
                }
                if item.is_object() && !is_reference(item) {
                    for field in ["position", "name"] {
                        let breadcrumb_item = props(item).get("item");
                        let mut item_name = false;
                        let mut item_unknown = false;
                        if field == "name" {
                            if let Some(entries) = breadcrumb_item {
                                for entry in values(entries) {
                                    let target = if is_reference(entry) {
                                        unique_reference(entry, base, ids).map(|(target, _)| target)
                                    } else {
                                        Some(entry)
                                    };
                                    match target {
                                        Some(target) if !unsupported_context(target) => {
                                            item_name |=
                                                props(target).get("name").is_some_and(present);
                                            if !target.is_object() {
                                                item_unknown = true;
                                            }
                                        }
                                        _ => item_unknown = true,
                                    }
                                }
                            }
                            if !item_name
                                && item_unknown
                                && !props(item).get("name").is_some_and(present)
                            {
                                add(
                                    out,
                                    "coverage",
                                    "breadcrumb_item_name_unknown",
                                    format!(
                                        "{item_path}{}/item",
                                        if item.get("properties").is_some() {
                                            "/properties"
                                        } else {
                                            ""
                                        }
                                    ),
                                    None,
                                    "Breadcrumb item name could not be determined without expansion",
                                );
                            }
                        }
                        if !(props(item).get(field).is_some_and(present)
                            || item_name
                            || item_unknown)
                        {
                            add(
                                out,
                                "provider_eligibility",
                                "missing_breadcrumb_property",
                                format!(
                                    "{item_path}{}/{field}",
                                    if item.get("properties").is_some() {
                                        "/properties"
                                    } else {
                                        ""
                                    }
                                ),
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
                if let Some(base) = base
                    && let Ok(u) = base.join(id)
                    && u.origin() == base.origin()
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
    let base = Url::parse(&page.url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some());
    if base.is_none() {
        add(
            &mut out,
            "coverage",
            "base_url_unavailable",
            String::new(),
            None,
            "Page URL is unavailable for local reference and resource resolution",
        );
    }
    let mut ids = HashMap::new();
    for (i, block) in observations.iter().enumerate() {
        if let (Some(v), Some(base)) = (&block.data, base.as_ref()) {
            collect_ids(v, &format!("/{i}/data"), base, &mut ids);
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
            check_item(v, &format!("{path}/data"), base.as_ref(), &ids, &mut out);
        }
    }
    // Headings are a partial source proxy, not proof of rendered visibility.
    let headings = page
        .headings
        .iter()
        .map(|(_, s)| s.to_lowercase())
        .collect::<Vec<_>>();
    if headings.iter().any(|h| !h.trim().is_empty()) {
        for (i, block) in observations.iter().enumerate() {
            if let Some(data) = &block.data {
                heading_candidates(data, &format!("/{i}/data"), &headings, &mut out);
            }
        }
    }
    if !headings.iter().any(|h| !h.trim().is_empty()) {
        add(
            &mut out,
            "coverage",
            "visible_source_unavailable",
            String::new(),
            None,
            "No nonempty source headings were retained; visible-content comparison is unavailable",
        );
    }
    if page.extraction_truncated {
        add(
            &mut out,
            "coverage",
            "extraction_truncated",
            String::new(),
            None,
            "Source observations were truncated; absence and identifier checks are incomplete",
        );
    }
    bound_bytes(&mut out);
    out
}

/// Look up an explicitly referenced HTTP resource in already collected observations.
/// A missing, blocked, or failed observation remains unknown and never triggers a fetch.
pub fn referenced_resource(
    page_url: &str,
    reference: &str,
    pages: &[Page],
) -> (&'static str, Option<u16>) {
    let Some(mut url) = Url::parse(page_url)
        .ok()
        .and_then(|base| base.join(reference).ok())
    else {
        return ("unknown", None);
    };
    url.set_fragment(None);
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
    if unsupported_context(v) {
        return;
    }
    if let Some(headline) = props(v).get("headline") {
        for (i, item) in values(headline).iter().enumerate() {
            if let Some(text) = item.as_str() {
                let h = text.trim().to_lowercase();
                if !h.is_empty()
                    && !headings.iter().any(|visible| {
                        !visible.trim().is_empty() && (visible.contains(&h) || h.contains(visible))
                    })
                {
                    let property = if v.get("properties").is_some() {
                        "properties/headline"
                    } else {
                        "headline"
                    };
                    let suffix = if headline.is_array() {
                        format!("/{i}")
                    } else {
                        String::new()
                    };
                    add(
                        out,
                        "human_review",
                        "headline_heading_mismatch_candidate",
                        format!("{path}/{property}{suffix}"),
                        Some(item),
                        "Headline differs from retained source headings; this is not proof of hidden or mismatched content",
                    );
                }
            }
        }
    }
    match v {
        Value::Object(m) => {
            for (key, child) in m {
                let child_path = format!("{path}/{}", escape(key));
                if key == "properties"
                    && let Some(properties) = child.as_object()
                {
                    for (property, value) in properties {
                        heading_candidates(
                            value,
                            &format!("{child_path}/{}", escape(property)),
                            headings,
                            out,
                        );
                    }
                } else {
                    heading_candidates(child, &child_path, headings, out);
                }
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
    if !page.html() || page.is_resource || page.blocked_by_robots || page.error.is_some() {
        let mut out = Vec::new();
        add(
            &mut out,
            "coverage",
            "page_unavailable",
            String::new(),
            None,
            "Successful HTML source was not available for structured-data validation",
        );
        return out;
    }
    let mut out = validate(page, &page.structured_data);
    for (i, block) in page.structured_data.iter().enumerate() {
        if let Some(v) = &block.data {
            resource_observations(v, &format!("/{i}/data"), page, pages, &mut out);
        }
    }
    bound_bytes(&mut out);
    out
}
fn resource_observations(
    v: &Value,
    path: &str,
    page: &Page,
    pages: &[Page],
    out: &mut Vec<Observation>,
) {
    if unsupported_context(v) {
        return;
    }
    match v {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}/{}", escape(key));
                if matches!(key.as_str(), "image" | "contentUrl" | "thumbnailUrl")
                    || (key == "url" && types(v).contains(&"ImageObject"))
                {
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

/// Inactive adapter for the proposed local checkpoint envelope. This type is
/// deliberately independent of the unmerged shared store interface.
#[derive(Debug, serde::Serialize)]
pub struct StructuredSidecar {
    pub collector: StructuredCollector,
    pub state: &'static str,
    pub omitted: Option<u64>,
    pub reason: Option<&'static str>,
    pub payload: serde_json::Value,
}
#[derive(Debug, serde::Serialize)]
pub struct StructuredCollector {
    pub id: &'static str,
    pub version: &'static str,
}
/// Build evidence without fetching or writing. The caller must bind it to the
/// exact source page URL and atomically checkpoint the page and all sidecars.
pub fn local_sidecar(page: &Page, pages: &[Page]) -> StructuredSidecar {
    let observations = validate_crawl(page, pages);
    let unavailable = observations.iter().any(|o| o.code == "page_unavailable");
    let partial = observations.iter().any(|o| {
        o.value_omitted
            || (o.category == "coverage" && !o.code.starts_with("local_"))
            || o.code == "unresolved_local_id"
            || (o.category == "resource"
                && (o.explanation.contains("state=unknown")
                    || o.explanation.contains("state=not_observed")))
    });
    StructuredSidecar {
        collector: StructuredCollector {
            id: "s04-structured-data",
            version: "s04-local-1",
        },
        state: if unavailable {
            "unavailable"
        } else if partial {
            "partial"
        } else {
            "observed"
        },
        omitted: if unavailable || partial {
            None
        } else {
            Some(0)
        },
        reason: if unavailable {
            Some("source_unavailable")
        } else if partial {
            Some("incomplete_evidence")
        } else {
            None
        },
        payload: serde_json::json!({"profile_version": PROFILE_VERSION, "observations": observations}),
    }
}
