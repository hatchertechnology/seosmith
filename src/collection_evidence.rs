//! Draft local occurrence capture. Not connected to reports or delivery.
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Occurrence {
    pub source: String,
    pub ordinal: usize,
    pub document_order: Option<usize>,
    pub raw_name: Option<String>,
    pub raw_value: Option<String>,
    pub raw_rel: Option<String>,
    pub raw_language: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Collection {
    pub occurrences: Vec<Occurrence>,
    pub complete: bool,
    pub omitted: usize,
}
const MAX_OCCURRENCES: usize = 2000;
const MAX_VALUE_BYTES: usize = 16_384;
// Each source gets a deterministic share of the provisional page budget.
const MAX_COLLECTION_BYTES: usize = 28 * 1024;
fn bounded(s: Option<&str>) -> bool {
    s.is_none_or(|s| s.len() <= MAX_VALUE_BYTES)
}
fn record(collection: &mut Collection, occurrence: Occurrence, bytes: &mut usize) {
    let size = serde_json::to_vec(&occurrence).map_or(usize::MAX, |v| v.len().saturating_add(1));
    if collection.omitted > 0
        || bytes.saturating_add(size) > MAX_COLLECTION_BYTES
        || collection.occurrences.len() >= MAX_OCCURRENCES
        || !bounded(occurrence.raw_name.as_deref())
        || !bounded(occurrence.raw_value.as_deref())
        || !bounded(occurrence.raw_rel.as_deref())
        || !bounded(occurrence.raw_language.as_deref())
    {
        collection.complete = false;
        collection.omitted += 1;
    } else {
        *bytes = bytes.saturating_add(size);
        collection.occurrences.push(occurrence);
    }
}
/// Collect HTML declarations in DOM order. Call only when decoded HTML is available;
/// decoding errors or an incomplete input must set `complete` false in the caller.
///
/// # Panics
/// Panics only if the static selector is invalid.
pub fn html_occurrences(html: &str, complete: bool) -> Collection {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("meta, link").expect("static selector");
    let mut result = Collection {
        occurrences: vec![],
        complete,
        omitted: 0,
    };
    let mut ordinals = std::collections::BTreeMap::<String, usize>::new();
    let mut bytes = 0usize;
    for (document_order, el) in doc.select(&selector).enumerate() {
        let name = el.value().name();
        let is_meta = name == "meta" && el.value().attr("name").is_some();
        let rel = el.value().attr("rel");
        let is_link = name == "link"
            && (rel.is_some_and(|r| {
                r.split_whitespace().any(|v| {
                    v.eq_ignore_ascii_case("canonical") || v.eq_ignore_ascii_case("alternate")
                })
            }) || el.value().attr("hreflang").is_some());
        if !is_meta && !is_link {
            continue;
        }
        let in_head = el
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|a| a.value().name() == "head");
        let source = format!("html_{}_{name}", if in_head { "head" } else { "body" });
        let ordinal = ordinals.entry(source.clone()).or_default();
        let occurrence = Occurrence {
            source,
            ordinal: *ordinal,
            document_order: Some(document_order),
            raw_name: el.value().attr("name").map(str::to_owned),
            raw_value: el
                .value()
                .attr(if is_meta { "content" } else { "href" })
                .map(str::to_owned),
            raw_rel: rel.map(str::to_owned),
            raw_language: el.value().attr("hreflang").map(str::to_owned),
        };
        *ordinal += 1;
        record(&mut result, occurrence, &mut bytes);
    }
    result
}
/// Preserve full individual HTTP field values; interpretation is a separate step.
pub fn http_occurrences(headers: &[(&str, &str)]) -> Collection {
    let mut result = Collection {
        occurrences: vec![],
        complete: true,
        omitted: 0,
    };
    let mut ordinals = std::collections::BTreeMap::<String, usize>::new();
    let mut bytes = 0usize;
    for &(name, value) in headers {
        if !name.eq_ignore_ascii_case("x-robots-tag") && !name.eq_ignore_ascii_case("link") {
            continue;
        }
        let source = name.to_ascii_lowercase();
        let ordinal = ordinals.entry(source.clone()).or_default();
        record(
            &mut result,
            Occurrence {
                source,
                ordinal: *ordinal,
                document_order: None,
                raw_name: Some(name.into()),
                raw_value: Some(value.into()),
                raw_rel: None,
                raw_language: None,
            },
            &mut bytes,
        );
        *ordinal += 1;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_repeated_raw_fields_and_empty_attributes() {
        let h = http_occurrences(&[
            ("X-Robots-Tag", "Googlebot: noindex"),
            ("X-Robots-Tag", "Googlebot: index"),
        ]);
        assert_eq!(h.occurrences.len(), 2);
        assert_eq!(h.occurrences[1].ordinal, 1);
        assert_eq!(
            h.occurrences[0].raw_value.as_deref(),
            Some("Googlebot: noindex")
        );
        let html = html_occurrences(
            "<html><head><link rel='canonical' href=''><meta name='GPTBot' content='noindex'></head></html>",
            true,
        );
        assert_eq!(html.occurrences.len(), 2);
        assert_eq!(html.occurrences[0].raw_value.as_deref(), Some(""));
    }
    #[test]
    fn limits_are_explicit() {
        let value = "a".repeat(MAX_VALUE_BYTES + 1);
        let result = http_occurrences(&[("Link", &value)]);
        assert!(!result.complete);
        assert_eq!(result.omitted, 1);
    }
}

/// A sidecar is never a report field. `None` means source unavailable, not empty.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PageCapture {
    pub version: String,
    pub url: String,
    pub state: String,
    pub robots: Vec<crate::robots_evidence::RobotsDecision>,
    pub robots_source: String,
    pub http: Option<Collection>,
    pub html: Option<Collection>,
}
impl PageCapture {
    pub fn unavailable(url: &str, reason: &str) -> Self {
        Self {
            version: "s02-s03-local-1".into(),
            url: url.into(),
            state: reason.into(),
            robots: vec![],
            robots_source: "unavailable".into(),
            http: None,
            html: None,
        }
    }
    pub fn from_response(url: &str, response: &crate::fetch::Response, html: bool) -> Self {
        let http = response.raw_headers.as_ref().map(|headers| {
            let pairs: Vec<_> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            http_occurrences(&pairs)
        });
        let html = html.then(|| {
            let content_type = response
                .headers
                .get("content-type")
                .map_or("", String::as_str);
            let (body, _, errors) = crate::extract::decode(&response.body, content_type);
            html_occurrences(&body, !errors)
        });
        Self {
            version: "s02-s03-local-1".into(),
            url: url.into(),
            state: "available".into(),
            robots: vec![],
            robots_source: "unavailable".into(),
            http,
            html,
        }
    }
}

#[cfg(test)]
mod transport_compat_tests {
    use super::*;
    #[test]
    fn absent_raw_header_support_is_not_an_empty_collection() {
        let mut response = crate::fetch::Response {
            status: 200,
            headers: std::collections::BTreeMap::new(),
            raw_headers: None,
            body: vec![],
            ttfb_ms: 0,
        };
        assert!(
            PageCapture::from_response("https://example.com/", &response, false)
                .http
                .is_none()
        );
        response.raw_headers = Some(vec![]);
        let captured = PageCapture::from_response("https://example.com/", &response, false)
            .http
            .unwrap();
        assert!(captured.complete);
        assert_eq!(captured.occurrences.len(), 0);
    }
}

/// Bound the serialized local sidecar before persistence. This deterministic
/// prefix reduction never turns omitted evidence into a complete collection.
/// The caller must reserve run-level coverage space separately and fail the
/// local job if even the minimum marker cannot fit.
pub fn enforce_page_budget(
    capture: &mut PageCapture,
    max_bytes: usize,
) -> Result<(), &'static str> {
    loop {
        let size = serde_json::to_vec(capture)
            .map_err(|_| "serialization_failed")?
            .len();
        if size <= max_bytes {
            return Ok(());
        }
        let mut removed = false;
        for collection in [&mut capture.html, &mut capture.http].into_iter().flatten() {
            if collection.occurrences.pop().is_some() {
                collection.complete = false;
                collection.omitted = collection.omitted.saturating_add(1);
                removed = true;
                break;
            }
        }
        if removed {
            continue;
        }
        if let Some(decision) = capture
            .robots
            .iter_mut()
            .rev()
            .find(|d| d.raw_line.is_some() || d.rule.is_some())
        {
            decision.raw_line = None;
            decision.rule = None;
            decision.matched_line = None;
            decision.provenance = "budget_omitted".into();
            continue;
        }
        return Err("minimum_marker_exceeds_budget");
    }
}
#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn source_capture_has_bounded_prefix_and_exact_omissions() {
        let value = "x".repeat(8000);
        let fields = vec![("Link", value.as_str()); 10];
        let collection = http_occurrences(&fields);
        assert!(!collection.complete);
        assert_eq!(collection.occurrences.len() + collection.omitted, 10);
        assert!(
            collection
                .occurrences
                .iter()
                .enumerate()
                .all(|(i, item)| item.ordinal == i)
        );
        assert!(serde_json::to_vec(&collection).unwrap().len() < 29 * 1024);
    }
    #[test]
    fn budget_preserves_prefix_and_marks_omissions() {
        let mut capture = PageCapture::unavailable("https://example.com/", "available");
        let values = [
            ("X-Robots-Tag", "a".repeat(500)),
            ("X-Robots-Tag", "b".repeat(500)),
        ];
        let pairs = values
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>();
        capture.http = Some(http_occurrences(&pairs));
        enforce_page_budget(&mut capture, 900).unwrap();
        let http = capture.http.as_ref().unwrap();
        assert!(!http.complete);
        assert_eq!(http.omitted, 1);
        assert_eq!(http.occurrences.len(), 1);
        assert!(serde_json::to_vec(&capture).unwrap().len() <= 900);
        assert_eq!(
            enforce_page_budget(&mut capture, 1),
            Err("minimum_marker_exceeds_budget")
        );
    }
}

/// Build a provisional local collector envelope; this is not a report or wire contract.
/// The byte budget includes the entire compact UTF-8 envelope, not only the payload.
pub fn local_sidecar(
    capture: &PageCapture,
    max_bytes: usize,
) -> Result<serde_json::Value, &'static str> {
    if capture.version != "s02-s03-local-1" {
        return Err("unsupported_capture_version");
    }
    if !matches!(
        capture.state.as_str(),
        "available" | "robots_blocked" | "fetch_failed" | "not_fetched"
    ) {
        return Err("invalid_capture_state");
    }
    if !url::Url::parse(&capture.url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    }) {
        return Err("invalid_capture_url");
    }
    let mut payload = capture.clone();
    let mut payload_budget = max_bytes.saturating_sub(256);
    loop {
        enforce_page_budget(&mut payload, payload_budget)?;
        let robots_known = matches!(
            payload.robots_source.as_str(),
            "robots_txt" | "not_found" | "http_denied_synthetic"
        ) && [
            "Seosmith",
            "Googlebot",
            "Bingbot",
            "GPTBot",
            "OAI-SearchBot",
            "ChatGPT-User",
        ]
        .iter()
        .all(|bot| {
            payload
                .robots
                .iter()
                .filter(|r| r.bot.eq_ignore_ascii_case(bot))
                .count()
                == 1
        }) && payload.robots.len() == 6
            && payload
                .robots
                .iter()
                .all(|r| match payload.robots_source.as_str() {
                    "robots_txt" => match r.provenance.as_str() {
                        "matched_rule" => {
                            r.matched_line.is_some() && r.rule.is_some() && r.raw_line.is_some()
                        }
                        "no_matching_rule" => {
                            r.allowed
                                && r.matched_line.is_none()
                                && r.rule.is_none()
                                && r.raw_line.is_none()
                        }
                        _ => false,
                    },
                    "not_found" | "http_denied_synthetic" => {
                        r.provenance == payload.robots_source
                            && r.allowed == (payload.robots_source == "not_found")
                            && r.matched_line.is_none()
                            && r.rule.is_none()
                            && r.raw_line.is_none()
                    }
                    _ => false,
                });
        let collections_complete = payload
            .http
            .as_ref()
            .is_some_and(|c| c.complete && c.omitted == 0)
            && payload
                .html
                .as_ref()
                .is_some_and(|c| c.complete && c.omitted == 0);
        let complete = payload.state == "available" && robots_known && collections_complete;
        let has_evidence =
            payload.http.is_some() || payload.html.is_some() || !payload.robots.is_empty();
        let state = if complete {
            "observed"
        } else if has_evidence {
            "partial"
        } else {
            "unavailable"
        };
        let omitted = match (&payload.http, &payload.html) {
            (Some(http), Some(html))
                if robots_known
                    && http.complete
                    && html.complete
                    && http.omitted == 0
                    && html.omitted == 0 =>
            {
                Some(http.omitted.saturating_add(html.omitted))
            }
            _ => None,
        };
        let reason = if complete {
            None
        } else if payload.state != "available" {
            Some(payload.state.as_str())
        } else {
            Some("incomplete_evidence")
        };
        let envelope = serde_json::json!({"collector":{"id":"s02-s03","version":"s02-s03-local-1"},"state":state,"omitted":omitted,"reason":reason,"payload":payload});
        let size = serde_json::to_vec(&envelope)
            .map_err(|_| "serialization_failed")?
            .len();
        if size <= max_bytes {
            return Ok(envelope);
        }
        let deficit = size - max_bytes;
        payload_budget = payload_budget
            .checked_sub(deficit)
            .ok_or("minimum_marker_exceeds_budget")?;
    }
}
#[cfg(test)]
mod sidecar_tests {
    use super::*;
    #[test]
    fn wrapper_binds_url_and_never_claims_blocked_content_complete() {
        let c = PageCapture::unavailable("https://example.com/blocked", "robots_blocked");
        let sidecar = local_sidecar(&c, 1024).unwrap();
        assert_eq!(sidecar["payload"]["url"], c.url);
        assert_eq!(sidecar["state"], "unavailable");
        assert!(sidecar["omitted"].is_null());
        assert_eq!(local_sidecar(&c, 1), Err("minimum_marker_exceeds_budget"));
        let mut observed = PageCapture::unavailable("https://example.com/", "available");
        observed.http = Some(http_occurrences(&[]));
        observed.html = Some(html_occurrences("<head></head>", true));
        observed.robots_source = "not_found".into();
        let url = url::Url::parse(&observed.url).unwrap();
        for bot in [
            "Seosmith",
            "Googlebot",
            "Bingbot",
            "GPTBot",
            "OAI-SearchBot",
            "ChatGPT-User",
        ] {
            let mut decision = crate::robots_evidence::evaluate("", bot, &url);
            decision.provenance = "not_found".into();
            observed.robots.push(decision);
        }
        let wrapped = local_sidecar(&observed, 4096).unwrap();
        assert_eq!(wrapped["state"], "observed");
        assert_eq!(wrapped["omitted"], 0);
        let mut missing_bot = observed.clone();
        missing_bot.robots.pop();
        let missing = local_sidecar(&missing_bot, 4096).unwrap();
        assert_eq!(missing["state"], "partial");
        assert!(missing["omitted"].is_null());
        observed.html.as_mut().unwrap().complete = false;
        let partial = local_sidecar(&observed, 4096).unwrap();
        assert_eq!(partial["state"], "partial");
        assert!(partial["omitted"].is_null());
        observed.html.as_mut().unwrap().complete = true;
        observed.html.as_mut().unwrap().omitted = 1;
        let inconsistent = local_sidecar(&observed, 4096).unwrap();
        assert_eq!(inconsistent["state"], "partial");
        assert!(inconsistent["omitted"].is_null());
        observed.version = "unsupported".into();
        assert_eq!(
            local_sidecar(&observed, 1024),
            Err("unsupported_capture_version")
        );
    }
}
