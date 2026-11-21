//! Local-only sitemap parsing with entry provenance; no report or wire integration.
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};
use std::io::Read;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub sitemap: String,
    pub ordinal: usize,
    pub location: Option<String>,
    pub lastmod: Option<String>,
    pub lastmod_state: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sitemap {
    pub source: String,
    pub index: bool,
    pub entries: Vec<Entry>,
    pub error: Option<String>,
}
/// Parse bounded XML or gzip-compressed XML, retaining raw entry values.
/// Entry locations must be absolute HTTP(S) URLs; scope is checked separately by the caller.
pub fn parse(source: &str, bytes: &[u8], max_bytes: usize, max_entries: usize) -> Sitemap {
    let mut result = Sitemap {
        source: source.into(),
        index: false,
        entries: vec![],
        error: None,
    };
    if bytes.len() > max_bytes {
        result.error = Some("size_limit".into());
        return result;
    }
    let decoded;
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut output = Vec::new();
        if flate2::read::GzDecoder::new(bytes)
            .take((max_bytes + 1) as u64)
            .read_to_end(&mut output)
            .is_err()
        {
            result.error = Some("invalid_gzip".into());
            return result;
        }
        if output.len() > max_bytes {
            result.error = Some("size_limit".into());
            return result;
        }
        decoded = output;
        decoded.as_slice()
    } else {
        bytes
    };
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    let mut depth = 0usize;
    let mut root = false;
    let mut root_count = 0usize;
    let mut entry_depth = None;
    let mut field: Option<&str> = None;
    let mut loc = String::new();
    let mut lastmod = String::new();
    let mut loc_count = 0usize;
    let mut lastmod_count = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                let name = e.local_name();
                let name = name.as_ref();
                if depth == 1 {
                    root_count += 1;
                    root = matches!(name, "urlset" | "sitemapindex");
                    result.index = name == "sitemapindex";
                } else if depth == 2
                    && ((result.index && name == "sitemap") || (!result.index && name == "url"))
                {
                    entry_depth = Some(depth);
                    loc.clear();
                    lastmod.clear();
                    loc_count = 0;
                    lastmod_count = 0;
                } else if entry_depth == Some(depth - 1) {
                    field = match name {
                        "loc" => {
                            loc_count += 1;
                            Some("loc")
                        }
                        "lastmod" => {
                            lastmod_count += 1;
                            Some("lastmod")
                        }
                        _ => None,
                    };
                }
            }
            Ok(Event::Text(e)) => {
                if let Some(f) = field {
                    match quick_xml::escape::unescape(e.as_ref()) {
                        Ok(s) => {
                            if f == "loc" {
                                loc.push_str(&s)
                            } else {
                                lastmod.push_str(&s)
                            }
                        }
                        Err(_) => {
                            result.error = Some("invalid_entity".into());
                            break;
                        }
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if let Some(f) = field {
                    if f == "loc" {
                        loc.push_str(e.as_ref())
                    } else {
                        lastmod.push_str(e.as_ref())
                    }
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if let Some(f) = field {
                    let entity = format!("&{};", e.as_ref());
                    match quick_xml::escape::unescape(&entity) {
                        Ok(s) => {
                            if f == "loc" {
                                loc.push_str(&s)
                            } else {
                                lastmod.push_str(&s)
                            }
                        }
                        Err(_) => {
                            result.error = Some("invalid_entity".into());
                            break;
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                if entry_depth == Some(depth) {
                    if result.entries.len() >= max_entries {
                        result.error = Some("entry_limit".into());
                        break;
                    }
                    let location = (loc_count > 0).then(|| loc.clone());
                    let error = match location.as_deref() {
                        _ if loc_count > 1 => Some("multiple_locations".into()),
                        None => Some("missing_location".into()),
                        Some(s) if s.trim().is_empty() => Some("empty_location".into()),
                        Some(s)
                            if !Url::parse(s.trim()).is_ok_and(|u| {
                                matches!(u.scheme(), "http" | "https")
                                    && u.host_str().is_some()
                                    && u.username().is_empty()
                                    && u.password().is_none()
                            }) =>
                        {
                            Some("invalid_location".into())
                        }
                        _ => None,
                    };
                    result.entries.push(Entry {
                        sitemap: source.into(),
                        ordinal: result.entries.len(),
                        location,
                        lastmod: (lastmod_count > 0).then(|| lastmod.clone()),
                        lastmod_state: if lastmod_count == 0 {
                            "absent"
                        } else if lastmod_count > 1 {
                            "multiple"
                        } else if lastmod.trim().is_empty() {
                            "empty"
                        } else if chrono::NaiveDate::parse_from_str(lastmod.trim(), "%Y-%m-%d")
                            .is_ok()
                            || chrono::DateTime::parse_from_rfc3339(lastmod.trim()).is_ok()
                        {
                            "valid"
                        } else {
                            "unrecognized"
                        }
                        .into(),
                        error,
                    });
                    entry_depth = None;
                }
                if entry_depth.is_some_and(|d| depth == d + 1) {
                    field = None;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => {
                if depth != 0 || !root || root_count != 1 {
                    result.error = Some("invalid_document".into());
                }
                break;
            }
            Err(_) => {
                result.error = Some("invalid_xml".into());
                break;
            }
            _ => {}
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_entries_and_errors() {
        let s = parse("https://example.com/sitemap.xml", br#"<urlset><url><loc>https://example.com/a</loc><lastmod>2026-01-01</lastmod></url><url><loc>bad</loc></url><url></url></urlset>"#, 1000, 10);
        assert_eq!(s.entries.len(), 3);
        assert_eq!(s.entries[0].lastmod.as_deref(), Some("2026-01-01"));
        assert_eq!(s.entries[1].error.as_deref(), Some("invalid_location"));
        assert_eq!(s.entries[2].error.as_deref(), Some("missing_location"));
        assert_eq!(s.entries[0].sitemap, s.source);
        assert_eq!(
            parse("x", b"<urlset><url>", 100, 10).error.as_deref(),
            Some("invalid_document")
        );
        assert_eq!(
            parse("x", b"<urlset/>", 2, 10).error.as_deref(),
            Some("size_limit")
        );
    }
    #[test]
    fn date_states_and_compression_are_explicit() {
        use std::io::Write;
        let body = b"<urlset><url><loc></loc><lastmod>bad</lastmod></url><url><loc>https://example.com/a</loc></url></urlset>";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(body).unwrap();
        let compressed = encoder.finish().unwrap();
        let parsed = parse("https://example.com/map", &compressed, 1024, 10);
        assert_eq!(parsed.entries[0].error.as_deref(), Some("empty_location"));
        assert_eq!(parsed.entries[0].lastmod_state, "unrecognized");
        assert_eq!(parsed.entries[1].lastmod_state, "absent");
        assert_eq!(
            parse("x", &compressed, 4, 10).error.as_deref(),
            Some("size_limit")
        );
    }
}
