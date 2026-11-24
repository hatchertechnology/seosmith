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
fn soft_404(page: &Page, response: Option<&Response>) -> Observation {
    let Some(response) = response else {
        return unavailable(
            "soft_404",
            if page.blocked_by_robots {
                "blocked_by_robots"
            } else {
                "response_unavailable"
            },
        );
    };
    let result = crate::s06_detector::soft_404(&crate::s06_detector::Source {
        bytes: Some(&response.body),
        status: Some(response.status),
        content_type: &page.content_type,
        complete: true,
    });
    let decision = match result.decision {
        crate::s06_detector::Decision::Candidate => "candidate",
        crate::s06_detector::Decision::NoCandidate => "no_candidate",
        crate::s06_detector::Decision::Unknown => "unknown",
    };
    let state = if result.evidence.is_none() {
        "unavailable"
    } else if result.decision == crate::s06_detector::Decision::Unknown {
        "partial"
    } else {
        "observed"
    };
    let evidence = result.evidence.map(|e| {
        json!({
            "source_sha256": e.source_sha256, "main_sha256": e.main_sha256,
            "title": e.title, "h1": e.h1, "excerpt": e.excerpt,
            "template_fraction": e.template_fraction
        })
    });
    let mut observation = observed(
        "soft_404",
        &json!({"semantics": crate::s06_detector::SEMANTICS,
        "decision": decision, "reason": result.reason, "evidence": evidence}),
        false,
    );
    if observation.data.is_some() {
        observation.state = state;
        observation.reason = (state != "observed").then_some(result.reason);
    }
    observation
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
    let mut value = json!({"version": 1, "collectors": [html, graph, resource], "omissions": ["http_occurrences_raw_headers_unavailable", "robots_provenance_not_captured", "sitemap_not_captured"]});
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
    let state = if page.blocked_by_robots {
        "robots_blocked"
    } else if response.is_none() {
        "fetch_failed"
    } else if page.html() {
        "available"
    } else {
        "content_unavailable"
    };
    let html = value["collectors"][0]["data"]
        .as_object()
        .and_then(|_| serde_json::from_value(value["collectors"][0]["data"].clone()).ok());
    let http = response.and_then(|r| r.local_http_occurrences.clone());
    let http_observation = match &http {
        Some(http) => observed("http_occurrences", http, !http.complete || http.omitted > 0),
        None => unavailable(
            "http_occurrences",
            if page.blocked_by_robots {
                "blocked_by_robots"
            } else if response.is_some() {
                "raw_headers_unavailable"
            } else {
                "response_unavailable"
            },
        ),
    };
    let http = http_observation
        .data
        .clone()
        .and_then(|v| serde_json::from_value(v).ok());
    let capture = collection_evidence::PageCapture {
        version: "s02-s03-local-1".into(),
        url: url.as_str().into(),
        state: state.into(),
        robots: decisions.clone(),
        robots_source: source.into(),
        http,
        html,
    };
    let effective: Vec<_> = [
        "Seosmith",
        "Googlebot",
        "Bingbot",
        "GPTBot",
        "OAI-SearchBot",
    ]
    .iter()
    .map(|bot| {
        crate::index_evidence::effective_indexing(
            bot,
            &capture,
            &crate::index_evidence::Intent::Unspecified,
        )
    })
    .collect();
    let indexing = observed(
        "effective_indexing",
        &json!({"state": state, "desired_policy": "unknown", "decisions": effective}),
        false,
    );
    value["collectors"].as_array_mut().unwrap().push(
        serde_json::to_value(observed(
            "robots",
            &json!({"source": source, "decisions": decisions}),
            false,
        ))
        .unwrap(),
    );
    value["collectors"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(http_observation).unwrap());
    value["collectors"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(indexing).unwrap());
    value["collectors"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(soft_404(page, response)).unwrap());
    value["omissions"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "http_occurrences_raw_headers_unavailable");
    value["omissions"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "robots_provenance_not_captured");
    value
}
pub(crate) fn budget_omission(url: &str) -> Value {
    let mut value = json!({"version": 1, "collectors": [unavailable("html_occurrences", "run_byte_limit"), unavailable("graph", "run_byte_limit"), unavailable("resource", "run_byte_limit"), unavailable("robots", "run_byte_limit"), unavailable("http_occurrences", "run_byte_limit"), unavailable("effective_indexing", "run_byte_limit"), unavailable("soft_404", "run_byte_limit")], "omissions": ["http_occurrences_raw_headers_unavailable", "robots_provenance_not_captured", "sitemap_not_captured"]});
    value["url"] = Value::Null;
    value["url_omission"] = json!("run_byte_limit");
    value["url_sha256"] = json!(crate::model::digest(url.as_bytes()));
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detector_respects_source_and_collector_limits() {
        let page = Page {
            content_type: "text/html".into(),
            ..Page::default()
        };
        let missing = soft_404(&page, None);
        assert_eq!(missing.reason, Some("response_unavailable"));
        let response = Response {
            status: 200,
            headers: std::collections::BTreeMap::new(),
            local_http_occurrences: None,
            body: format!("<title>{}</title><main>content</main>", "x".repeat(25_000)).into_bytes(),
            ttfb_ms: 0,
        };
        assert_eq!(
            soft_404(&page, Some(&response)).reason,
            Some("collector_byte_limit")
        );
        let text_limited = Response {
            body: format!("<main>{}</main>", "x".repeat(32 * 1024 + 1)).into_bytes(),
            status: 200,
            headers: std::collections::BTreeMap::new(),
            local_http_occurrences: None,
            ttfb_ms: 0,
        };
        let unavailable = soft_404(&page, Some(&text_limited));
        assert_eq!(unavailable.state, "unavailable");
        assert_eq!(unavailable.reason, Some("text exceeds limit"));
        assert_eq!(
            unavailable.data.unwrap()["semantics"],
            crate::s06_detector::SEMANTICS
        );
        let partial_over_cap = Response {
            body: format!(
                "<title>{}</title><main>Loading...</main>",
                "x".repeat(25_000)
            )
            .into_bytes(),
            ..response
        };
        assert_eq!(
            soft_404(&page, Some(&partial_over_cap)).reason,
            Some("collector_byte_limit")
        );
        assert_eq!(
            budget_omission("https://example.test/")["collectors"][6]["reason"],
            "run_byte_limit"
        );
    }
    #[test]
    fn indexing_preserves_unknown_when_headers_are_missing_invalid_or_limited() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let url = Url::parse("https://example.test/").unwrap();
        let page = Page {
            status: Some(200),
            content_type: "text/html".into(),
            ..Page::default()
        };
        let mut response = Response {
            status: 200,
            headers: std::collections::BTreeMap::new(),
            local_http_occurrences: None,
            body: b"<html></html>".to_vec(),
            ttfb_ms: 0,
        };
        let (_, complete) = crate::fetch::extract_headers(&HeaderMap::new());
        response.local_http_occurrences = Some(complete);
        let control = capture_robots(&url, &page, Some(&response), "", "not_found");
        assert_eq!(
            control["collectors"][5]["data"]["decisions"][1]["directives"]["index"],
            "Allowed"
        );
        response.local_http_occurrences = None;
        for mode in 0..3 {
            if mode > 0 {
                let mut headers = HeaderMap::new();
                let value = if mode == 1 {
                    HeaderValue::from_bytes(&[0xff]).unwrap()
                } else {
                    HeaderValue::from_str(&"x".repeat(16_385)).unwrap()
                };
                headers.append("x-robots-tag", value);
                let (ordinary, evidence) = crate::fetch::extract_headers(&headers);
                response.headers = ordinary;
                response.local_http_occurrences = Some(evidence);
            }
            let value = capture_robots(&url, &page, Some(&response), "", "not_found");
            assert_eq!(
                value["collectors"][5]["data"]["decisions"][1]["directives"]["index"],
                "Unknown"
            );
            if mode == 0 {
                assert_eq!(value["collectors"][4]["reason"], "raw_headers_unavailable");
            } else {
                assert_eq!(value["collectors"][4]["state"], "partial");
            }
        }
    }
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
