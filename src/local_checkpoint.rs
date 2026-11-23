//! Bounded local observations, never included in delivery reports.
use crate::{collection_evidence, fetch::Response, graph_evidence, model::Page, resource_evidence};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

const COLLECTOR_LIMIT: usize = 24 * 1024;

#[derive(Serialize)]
struct Observation {
    collector: &'static str,
    version: u8,
    state: &'static str,
    reason: Option<&'static str>,
    data: Option<Value>,
}
fn unavailable(collector: &'static str, reason: &'static str) -> Observation {
    Observation {
        collector,
        version: 1,
        state: "unavailable",
        reason: Some(reason),
        data: None,
    }
}
fn observed<T: Serialize>(collector: &'static str, value: &T, partial: bool) -> Observation {
    let data = serde_json::to_value(value).expect("local evidence serializes");
    if serde_json::to_vec(&data)
        .expect("local evidence serializes")
        .len()
        > COLLECTOR_LIMIT
    {
        return unavailable(collector, "collector_byte_limit");
    }
    Observation {
        collector,
        version: 1,
        state: if partial { "partial" } else { "observed" },
        reason: partial.then_some("source_or_extraction_incomplete"),
        data: Some(data),
    }
}
/// The collector set is fixed for every page, including unavailable sources.
pub(crate) fn capture(url: &Url, page: &Page, response: Option<&Response>) -> Value {
    let (html, graph, resource) = if let Some(response) = response {
        let resource = observed(
            "resource",
            &resource_evidence::resource(url.as_str(), &response.headers, &response.body),
            false,
        );
        if page.html() {
            let (body, _, errors) = crate::extract::decode(&response.body, &page.content_type);
            let collection = collection_evidence::html_occurrences(&body, !errors);
            let graph = graph_evidence::extract(url, &body, response.status);
            let partial = errors || !graph.extraction_complete;
            (
                observed("html_occurrences", &collection, !collection.complete),
                observed("graph", &graph, partial),
                resource,
            )
        } else {
            (
                unavailable("html_occurrences", "not_html"),
                unavailable("graph", "not_html"),
                resource,
            )
        }
    } else {
        let reason = if page.blocked_by_robots {
            "blocked_by_robots"
        } else {
            "response_unavailable"
        };
        (
            unavailable("html_occurrences", reason),
            unavailable("graph", reason),
            unavailable("resource", reason),
        )
    };
    json!({"version": 1, "url": url.as_str(), "collectors": [html, graph, resource], "omissions": ["http_occurrences_raw_headers_unavailable", "robots_provenance_not_captured", "sitemap_not_captured", "content_detector_not_integrated"]})
}
pub(crate) fn budget_omission(url: &str) -> Value {
    json!({"version": 1, "url": url, "collectors": [unavailable("html_occurrences", "run_byte_limit"), unavailable("graph", "run_byte_limit"), unavailable("resource", "run_byte_limit")], "omissions": ["http_occurrences_raw_headers_unavailable", "robots_provenance_not_captured", "sitemap_not_captured", "content_detector_not_integrated"]})
}
