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
fn bounded(s: Option<&str>) -> bool {
    s.is_none_or(|s| s.len() <= MAX_VALUE_BYTES)
}
fn record(collection: &mut Collection, occurrence: Occurrence) {
    if collection.occurrences.len() >= MAX_OCCURRENCES
        || !bounded(occurrence.raw_name.as_deref())
        || !bounded(occurrence.raw_value.as_deref())
        || !bounded(occurrence.raw_rel.as_deref())
        || !bounded(occurrence.raw_language.as_deref())
    {
        collection.complete = false;
        collection.omitted += 1;
    } else {
        collection.occurrences.push(occurrence);
    }
}
/// Collect HTML declarations in DOM order. Call only when decoded HTML is available;
/// decoding errors or an incomplete input must set `complete` false in the caller.
pub fn html_occurrences(html: &str, complete: bool) -> Collection {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("meta, link").expect("static selector");
    let mut result = Collection {
        occurrences: vec![],
        complete,
        omitted: 0,
    };
    let mut ordinals = std::collections::BTreeMap::<String, usize>::new();
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
        record(&mut result, occurrence);
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
