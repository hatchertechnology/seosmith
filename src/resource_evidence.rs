//! Experimental offline resource observations; no fetching or report delivery.
use crate::model::digest;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use url::Url;

#[derive(Debug, Serialize)]
pub struct ResourceEvidence {
    pub url: String,
    pub decoded_body_bytes: usize,
    /// A header is an estimate of the encoded representation, not a measured transfer.
    pub declared_content_length: Option<u64>,
    pub transfer_bytes: Option<u64>,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub cache_control: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body_hash: String,
    pub detected_image_format: Option<&'static str>,
    pub image_dimensions: Option<(u32, u32)>,
}
/// Body is the decoded bytes returned by the transport. Transfer size is unavailable.
pub fn resource(url: &str, headers: &BTreeMap<String, String>, body: &[u8]) -> ResourceEvidence {
    let (format, dimensions) = image_info(body);
    ResourceEvidence {
        url: url.into(),
        decoded_body_bytes: body.len(),
        declared_content_length: headers.get("content-length").and_then(|v| v.parse().ok()),
        transfer_bytes: None,
        content_type: headers.get("content-type").cloned(),
        content_encoding: headers.get("content-encoding").cloned(),
        cache_control: headers.get("cache-control").cloned(),
        etag: headers.get("etag").cloned(),
        last_modified: headers.get("last-modified").cloned(),
        body_hash: digest(body),
        detected_image_format: format,
        image_dimensions: dimensions,
    }
}
fn image_info(b: &[u8]) -> (Option<&'static str>, Option<(u32, u32)>) {
    if b.len() >= 24 && b.starts_with(b"\x89PNG\r\n\x1a\n") && &b[12..16] == b"IHDR" {
        return (
            Some("png"),
            Some((
                u32::from_be_bytes(b[16..20].try_into().unwrap()),
                u32::from_be_bytes(b[20..24].try_into().unwrap()),
            )),
        );
    }
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        return (
            Some("gif"),
            Some((
                u16::from_le_bytes([b[6], b[7]]).into(),
                u16::from_le_bytes([b[8], b[9]]).into(),
            )),
        );
    }
    if b.starts_with(&[0xff, 0xd8, 0xff]) {
        return (Some("jpeg"), None);
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return (Some("webp"), None);
    }
    (None, None)
}

/// Propagate static resource dependencies back to HTML pages, with cycles bounded by visited URLs.
pub fn affected_pages(pages: &[crate::model::Page]) -> BTreeMap<String, BTreeSet<String>> {
    let by_url: BTreeMap<_, _> = pages.iter().map(|p| (p.url.as_str(), p)).collect();
    let mut affected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for page in pages.iter().filter(|p| !p.is_resource && p.html()) {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<&str> = page
            .relationships
            .iter()
            .filter(|r| r.resource)
            .map(|r| r.url.as_str())
            .collect();
        while let Some(target) = queue.pop_front() {
            if !seen.insert(target) {
                continue;
            }
            affected
                .entry(target.to_owned())
                .or_default()
                .insert(page.url.clone());
            if let Some(resource) = by_url.get(target) {
                queue.extend(
                    resource
                        .relationships
                        .iter()
                        .filter(|r| r.resource)
                        .map(|r| r.url.as_str()),
                );
            }
        }
    }
    affected
}

#[derive(Debug, Serialize)]
pub struct DuplicateResourceGroup {
    pub decoded_body_hash: String,
    pub urls: Vec<String>,
}
/// Equality of decoded bodies is an observation, not proof that assets are interchangeable.
pub fn duplicate_resources(resources: &[ResourceEvidence]) -> Vec<DuplicateResourceGroup> {
    let mut groups: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in resources {
        groups.entry(&r.body_hash).or_default().insert(&r.url);
    }
    groups
        .into_iter()
        .filter(|(_, urls)| urls.len() > 1)
        .map(|(hash, urls)| DuplicateResourceGroup {
            decoded_body_hash: hash.into(),
            urls: urls.into_iter().map(str::to_owned).collect(),
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct ResourceByteGroup {
    pub origin: Option<String>,
    pub content_type: Option<String>,
    pub resource_count: usize,
    pub decoded_body_bytes: u64,
    pub complete: bool,
}
/// Totals only observed decoded bodies, grouped by origin and declared MIME type.
pub fn resource_byte_groups(
    resources: &[ResourceEvidence],
    collection_complete: bool,
) -> Vec<ResourceByteGroup> {
    type ByteGroupMap = BTreeMap<(Option<String>, Option<String>), (usize, u64, bool)>;
    let mut groups: ByteGroupMap = BTreeMap::new();
    for r in resources {
        let origin = Url::parse(&r.url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .map(|u| u.origin().ascii_serialization());
        let content_type = r
            .content_type
            .as_ref()
            .map(|s| {
                s.split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            })
            .filter(|s| !s.is_empty());
        let group = groups
            .entry((origin, content_type))
            .or_insert((0, 0, collection_complete));
        group.0 += 1;
        match u64::try_from(r.decoded_body_bytes)
            .ok()
            .and_then(|n| group.1.checked_add(n))
        {
            Some(sum) => group.1 = sum,
            None => group.2 = false,
        }
    }
    groups
        .into_iter()
        .map(
            |((origin, content_type), (resource_count, decoded_body_bytes, complete))| {
                ResourceByteGroup {
                    origin,
                    content_type,
                    resource_count,
                    decoded_body_bytes,
                    complete,
                }
            },
        )
        .collect()
}

/// A retained static HTML occurrence, not a browser-selected resource.
#[derive(Debug, Serialize)]
pub struct ResourceReference {
    pub element: String,
    pub raw_src: Option<String>,
    /// Raw relation tokens, retained without classifying the link.
    pub raw_rel: Option<String>,
    pub raw_srcset: Option<String>,
    pub media: Option<String>,
    pub declared_type: Option<String>,
    pub as_attribute: Option<String>,
    pub crossorigin: Option<String>,
    pub sizes: Option<String>,
    pub loading: Option<String>,
    pub fetchpriority: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResourceReferences {
    pub source_url: String,
    pub occurrences: Vec<ResourceReference>,
    /// False when the occurrence or attribute limit was reached.
    pub complete: bool,
}

/// Retain at most 2,000 occurrences and 2,048 characters per attribute.
///
/// # Panics
/// Panics if the built-in CSS selector is invalid.
pub fn references(source_url: &str, html: &str) -> ResourceReferences {
    let doc = scraper::Html::parse_document(html);
    let selector =
        scraper::Selector::parse("img,source,script[src],link[href]").expect("static selector");
    let mut occurrences = Vec::new();
    let mut complete = true;
    for el in doc.select(&selector) {
        if occurrences.len() == 2000 {
            complete = false;
            break;
        }
        let names = [
            "src",
            "href",
            "rel",
            "srcset",
            "media",
            "type",
            "as",
            "crossorigin",
            "sizes",
            "loading",
            "fetchpriority",
            "width",
            "height",
        ];
        if names
            .iter()
            .filter_map(|name| el.value().attr(name))
            .any(|v| v.chars().count() > 2048)
        {
            complete = false;
        }
        let attr = |name| {
            el.value()
                .attr(name)
                .map(|v| v.chars().take(2048).collect())
        };
        occurrences.push(ResourceReference {
            element: el.value().name().into(),
            raw_src: attr("src").or_else(|| attr("href")),
            raw_rel: attr("rel"),
            raw_srcset: attr("srcset"),
            media: attr("media"),
            declared_type: attr("type"),
            as_attribute: attr("as"),
            crossorigin: attr("crossorigin"),
            sizes: attr("sizes"),
            loading: attr("loading"),
            fetchpriority: attr("fetchpriority"),
            width: attr("width"),
            height: attr("height"),
        });
    }
    ResourceReferences {
        source_url: source_url.into(),
        occurrences,
        complete,
    }
}
