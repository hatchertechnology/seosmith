//! Bounded local observations, never included in delivery reports.
use crate::{collection_evidence, fetch::Response, graph_evidence, model::Page, resource_evidence};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

const COLLECTOR_LIMIT: usize = 24 * 1024;
const URL_COPY_LIMIT: usize = 8192;

fn set_url_identity(value: &mut Value, url: &str) {
    if url.len() <= URL_COPY_LIMIT {
        value["url"] = json!(url);
    } else {
        value["url"] = Value::Null;
        value["url_omission"] = json!("url_copy_byte_limit");
        value["url_sha256"] = json!(crate::model::digest(url.as_bytes()));
    }
}
pub(crate) fn url_identity_matches(value: &Value, url: &str) -> bool {
    if url.len() <= URL_COPY_LIMIT {
        value.get("url").and_then(Value::as_str) == Some(url)
    } else {
        value.get("url") == Some(&Value::Null)
            && value.get("url_omission").and_then(Value::as_str) == Some("url_copy_byte_limit")
            && value.get("url_sha256").and_then(Value::as_str)
                == Some(crate::model::digest(url.as_bytes()).as_str())
    }
}

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
    let mut value = json!({"version": 1, "collectors": [html, graph, resource], "omissions": ["http_occurrences_raw_headers_unavailable", "robots_provenance_not_captured", "sitemap_not_captured", "content_detector_not_integrated"]});
    set_url_identity(&mut value, url.as_str());
    value
}
/// Capture the decisions from the same policy bytes used by admission.
pub(crate) fn capture_robots(
    url: &Url,
    page: &Page,
    response: Option<&Response>,
    body: &str,
    source: &str,
) -> Value {
    let mut value = capture(url, page, response);
    let decisions: Vec<_> = [
        "Seosmith",
        "Googlebot",
        "Bingbot",
        "GPTBot",
        "OAI-SearchBot",
    ]
    .iter()
    .map(|bot| {
        let mut d = crate::robots_evidence::evaluate(body, bot, url);
        if source != "robots_txt" {
            d.matched_line = None;
            d.rule = None;
            d.raw_line = None;
            d.provenance = source.into();
        } else if d.rule.as_ref().is_some_and(|v| v.len() > 1024)
            || d.raw_line.as_ref().is_some_and(|v| v.len() > 1024)
        {
            d.rule = None;
            d.raw_line = None;
            d.provenance = "matched_provenance_byte_limit".into();
        }
        d
    })
    .collect();
    value["collectors"].as_array_mut().unwrap().push(
        serde_json::to_value(observed(
            "robots",
            &json!({"source": source, "decisions": decisions}),
            false,
        ))
        .unwrap(),
    );
    value["omissions"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "robots_provenance_not_captured");
    value
}
pub(crate) fn budget_omission(url: &str) -> Value {
    let mut value = json!({"version": 1, "collectors": [unavailable("html_occurrences", "run_byte_limit"), unavailable("graph", "run_byte_limit"), unavailable("resource", "run_byte_limit"), unavailable("robots", "run_byte_limit")], "omissions": ["http_occurrences_raw_headers_unavailable", "robots_provenance_not_captured", "sitemap_not_captured", "content_detector_not_integrated"]});
    value["url"] = Value::Null;
    value["url_omission"] = json!("run_byte_limit");
    value["url_sha256"] = json!(crate::model::digest(url.as_bytes()));
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn robots_provenance_is_bounded_without_changing_decision() {
        let path = format!("/{}", "a".repeat(1100));
        let url = Url::parse(&format!("https://example.test{path}")).unwrap();
        let body = format!("User-agent: *\nDisallow: {path}\n");
        let value = capture_robots(&url, &Page::default(), None, &body, "robots_txt");
        let decision = &value["collectors"][3]["data"]["decisions"][0];
        assert_eq!(decision["allowed"], false);
        assert_eq!(decision["provenance"], "matched_provenance_byte_limit");
        assert!(decision["rule"].is_null());
        assert!(decision["raw_line"].is_null());
    }
    #[test]
    fn long_url_budget_marker_is_bounded_and_bound_to_full_identity() {
        let url = format!("https://example.test/{}", "x".repeat(100_000));
        let marker = budget_omission(&url);
        assert_eq!(marker["url_sha256"], crate::model::digest(url.as_bytes()));
        assert_eq!(marker["url_omission"], "run_byte_limit");
        let short = budget_omission("https://example.test/");
        assert_eq!(
            serde_json::to_vec(&marker).unwrap().len(),
            serde_json::to_vec(&short).unwrap().len()
        );
        assert!(serde_json::to_vec(&marker).unwrap().len() < 2048);
    }
}
