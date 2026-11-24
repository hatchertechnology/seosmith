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
    /// None means the number of omitted entries is not known.
    pub omitted_entries: Option<usize>,
    pub error: Option<String>,
}
/// Parse bounded XML or gzip-compressed XML, retaining raw entry values.
/// Entry locations must be absolute HTTP(S) URLs; scope is checked separately by the caller.
pub fn parse(source: &str, bytes: &[u8], max_bytes: usize, max_entries: usize) -> Sitemap {
    let mut result = Sitemap {
        source: source.into(),
        index: false,
        entries: vec![],
        omitted_entries: Some(0),
        error: None,
    };
    if bytes.len() > max_bytes {
        result.error = Some("size_limit".into());
        result.omitted_entries = None;
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
            result.omitted_entries = None;
            return result;
        }
        if output.len() > max_bytes {
            result.error = Some("size_limit".into());
            result.omitted_entries = None;
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
    let mut unrepresented_location = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                let name = e.local_name();
                let name = name.as_ref();
                // A value-only comparison cannot locate a nested occurrence,
                // even when it duplicates a retained direct location.
                if name == "loc" && entry_depth != Some(depth - 1) {
                    unrepresented_location = true;
                }
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
                    if let Ok(s) = quick_xml::escape::unescape(e.as_ref()) {
                        if f == "loc" {
                            loc.push_str(&s);
                        } else {
                            lastmod.push_str(&s);
                        }
                    } else {
                        result.error = Some("invalid_entity".into());
                        break;
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if let Some(f) = field {
                    if f == "loc" {
                        loc.push_str(e.as_ref());
                    } else {
                        lastmod.push_str(e.as_ref());
                    }
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if let Some(f) = field {
                    let entity = format!("&{};", e.as_ref());
                    if let Ok(s) = quick_xml::escape::unescape(&entity) {
                        if f == "loc" {
                            loc.push_str(&s);
                        } else {
                            lastmod.push_str(&s);
                        }
                    } else {
                        result.error = Some("invalid_entity".into());
                        break;
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
    if unrepresented_location && result.error.is_none() {
        result.error = Some("consumed_provenance_unavailable".into());
    }
    if result.error.is_some() {
        result.omitted_entries = None;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_entries_and_errors() {
        let s = parse("https://example.com/sitemap.xml", br"<urlset><url><loc>https://example.com/a</loc><lastmod>2026-01-01</lastmod></url><url><loc>bad</loc></url><url></url></urlset>", 1000, 10);
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryScope {
    InScope,
    OutsideScope,
    Invalid,
}
/// A sitemap entry outside the crawl host is not an observed passing target.
pub fn entry_scope(entry: &Entry, scope: &Url) -> EntryScope {
    if entry.error.is_some() {
        return EntryScope::Invalid;
    }
    let Some(url) = entry
        .location
        .as_deref()
        .and_then(|v| Url::parse(v.trim()).ok())
    else {
        return EntryScope::Invalid;
    };
    if matches!(url.scheme(), "http" | "https")
        && url.host_str() == scope.host_str()
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
    {
        EntryScope::InScope
    } else {
        EntryScope::OutsideScope
    }
}
#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn scope_is_independent_of_valid_url_syntax() {
        let scope = Url::parse("https://example.com/").unwrap();
        let doc=parse("https://example.com/s.xml", b"<urlset><url><loc>https://example.com/a</loc></url><url><loc>https://outside.example/a</loc></url><url><loc>broken</loc></url></urlset>",1000,10);
        assert_eq!(
            doc.entries
                .iter()
                .map(|e| entry_scope(e, &scope))
                .collect::<Vec<_>>(),
            vec![
                EntryScope::InScope,
                EntryScope::OutsideScope,
                EntryScope::Invalid
            ]
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryPath {
    pub documents: Vec<String>,
    /// Ordinal of the index entry for each consecutive document pair.
    pub index_entry_ordinals: Vec<usize>,
    pub entry_ordinal: usize,
    pub location: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryOrigins {
    pub paths: Vec<EntryPath>,
    pub complete: bool,
}
/// Explain every observed path from a root index to an entry. Missing, failed,
/// out-of-scope, cyclic or over-budget document traversal makes coverage partial.
pub fn entry_origins(
    root: &str,
    target: &str,
    documents: &std::collections::BTreeMap<String, Sitemap>,
    scope: &Url,
    limit: usize,
) -> EntryOrigins {
    let Some(root) = crate::model::normalize(scope, root)
        .filter(|u| u.host_str() == scope.host_str() && u.port().is_none())
    else {
        return EntryOrigins {
            paths: Vec::new(),
            complete: false,
        };
    };
    let root = root.to_string();
    let Some(target) = crate::model::normalize(scope, target)
        .filter(|u| u.host_str() == scope.host_str() && u.port().is_none())
    else {
        return EntryOrigins {
            paths: Vec::new(),
            complete: false,
        };
    };
    let target = target.to_string();
    let mut paths = Vec::new();
    let mut complete = true;
    let mut queue = std::collections::VecDeque::from([(root.clone(), vec![root], Vec::new())]);
    let mut visits = 0usize;
    while let Some((document, path, index_ordinals)) = queue.pop_front() {
        if visits >= limit {
            complete = false;
            break;
        }
        visits += 1;
        let Some(map) = documents.get(&document) else {
            complete = false;
            continue;
        };
        if map.source != document {
            complete = false;
            continue;
        }
        if map.error.is_some() || map.omitted_entries != Some(0) {
            complete = false;
        }
        for entry in &map.entries {
            if entry.sitemap != document {
                complete = false;
                continue;
            }
            let Some(raw) = entry.location.as_deref() else {
                complete = false;
                continue;
            };
            match entry_scope(entry, scope) {
                EntryScope::Invalid | EntryScope::OutsideScope => {
                    complete = false;
                    continue;
                }
                EntryScope::InScope => {}
            }
            let Some(mut parsed) = Url::parse(raw.trim()).ok() else {
                complete = false;
                continue;
            };
            parsed.set_fragment(None);
            let location = parsed.to_string();
            if map.index {
                if path.iter().any(|p| p == &location) || path.len() >= 128 {
                    complete = false;
                    continue;
                }
                let mut next = path.clone();
                next.push(location.clone());
                if queue.len() >= limit {
                    complete = false;
                    continue;
                }
                let mut next_ordinals = index_ordinals.clone();
                next_ordinals.push(entry.ordinal);
                queue.push_back((location, next, next_ordinals));
            } else if location == target {
                paths.push(EntryPath {
                    documents: path.clone(),
                    index_entry_ordinals: index_ordinals.clone(),
                    entry_ordinal: entry.ordinal,
                    location,
                });
                if paths.len() >= limit {
                    complete = false;
                    break;
                }
            }
        }
    }
    EntryOrigins { paths, complete }
}
#[cfg(test)]
mod origin_tests {
    use super::*;
    #[test]
    fn omissions_and_mismatched_provenance_cannot_claim_complete_paths() {
        let scope = Url::parse("https://example.com/").unwrap();
        let root = "https://example.com/map.xml";
        let target = "https://example.com/a";
        let document = parse(
            root,
            b"<urlset><url><loc>https://example.com/a</loc></url></urlset>",
            1000,
            10,
        );
        let mut docs = std::collections::BTreeMap::from([(root.into(), document)]);
        docs.get_mut(root).unwrap().omitted_entries = None;
        let result = entry_origins(root, target, &docs, &scope, 10);
        assert_eq!(result.paths.len(), 1);
        assert!(!result.complete);
        let doc = docs.get_mut(root).unwrap();
        doc.omitted_entries = Some(0);
        doc.entries[0].sitemap = "https://example.com/other.xml".into();
        let result = entry_origins(root, target, &docs, &scope, 10);
        assert!(result.paths.is_empty());
        assert!(!result.complete);
        docs.get_mut(root).unwrap().source = "https://example.com/other.xml".into();
        assert!(!entry_origins(root, target, &docs, &scope, 10).complete);
    }
    #[test]
    fn repeated_and_nested_index_entries_retain_each_hop_ordinal() {
        let scope = Url::parse("https://example.com/").unwrap();
        let root = "https://example.com/index.xml";
        let child = "https://example.com/child.xml";
        let leaf = "https://example.com/leaf.xml";
        let target = "https://example.com/a";
        let index = parse(root, format!("<sitemapindex><sitemap><loc>{child}</loc></sitemap><sitemap><loc>{child}</loc></sitemap></sitemapindex>").as_bytes(), 2000, 10);
        let middle = parse(
            child,
            format!("<sitemapindex><sitemap><loc>{leaf}</loc></sitemap></sitemapindex>").as_bytes(),
            2000,
            10,
        );
        let last = parse(
            leaf,
            format!("<urlset><url><loc>{target}</loc></url></urlset>").as_bytes(),
            2000,
            10,
        );
        let docs = std::collections::BTreeMap::from([
            (root.into(), index),
            (child.into(), middle),
            (leaf.into(), last),
        ]);
        let result = entry_origins(root, target, &docs, &scope, 10);
        assert!(result.complete);
        assert_eq!(result.paths.len(), 2);
        assert_eq!(result.paths[0].documents, vec![root, child, leaf]);
        assert_eq!(result.paths[0].index_entry_ordinals, vec![0, 0]);
        assert_eq!(result.paths[1].index_entry_ordinals, vec![1, 0]);
        assert_eq!(result.paths[0].entry_ordinal, 0);
    }
    #[test]
    fn multiple_index_paths_and_missing_documents_are_explicit() {
        let scope = Url::parse("https://example.com/").unwrap();
        let root = "https://example.com/index.xml";
        let one = "https://example.com/one.xml";
        let two = "https://example.com/two.xml";
        let index=parse(root,format!("<sitemapindex><sitemap><loc>{one}</loc></sitemap><sitemap><loc>{two}</loc></sitemap></sitemapindex>").as_bytes(),2000,10);
        let body = b"<urlset><url><loc>https://example.com/a</loc></url></urlset>";
        let mut docs = std::collections::BTreeMap::from([
            (root.into(), index),
            (one.into(), parse(one, body, 1000, 10)),
            (two.into(), parse(two, body, 1000, 10)),
        ]);
        let origins = entry_origins(root, "https://example.com/a", &docs, &scope, 10);
        assert!(origins.complete);
        assert_eq!(origins.paths.len(), 2);
        assert_eq!(origins.paths[0].documents, vec![root, one]);
        docs.remove(two);
        assert!(!entry_origins(root, "https://example.com/a", &docs, &scope, 10).complete);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageObservation {
    pub blocked: bool,
    pub fetched: bool,
    pub status: Option<u16>,
    pub noindex: Option<bool>,
    /// None is unavailable, Some(None) is an observed absence.
    pub canonical: Option<Option<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryObservation {
    Blocked,
    Noindex,
    Noncanonical,
    HttpError,
    Redirect,
    Available,
    Unknown,
    OutsideScope,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryEvaluation {
    pub origins: EntryOrigins,
    pub observations: Vec<EntryObservation>,
}
/// Reports observations only; intentional policy exclusions are evaluated separately.
pub fn evaluate_entry(
    root: &str,
    target: &str,
    documents: &std::collections::BTreeMap<String, Sitemap>,
    pages: &std::collections::BTreeMap<String, PageObservation>,
    scope: &Url,
    limit: usize,
) -> EntryEvaluation {
    let origins = entry_origins(root, target, documents, scope, limit);
    let Some(url) = crate::model::normalize(scope, target) else {
        return EntryEvaluation {
            origins,
            observations: vec![EntryObservation::Unknown],
        };
    };
    if url.host_str() != scope.host_str() || url.port().is_some() {
        return EntryEvaluation {
            origins,
            observations: vec![EntryObservation::OutsideScope],
        };
    }
    let Some(page) = pages.get(url.as_str()) else {
        return EntryEvaluation {
            origins,
            observations: vec![EntryObservation::Unknown],
        };
    };
    if page.blocked {
        return EntryEvaluation {
            origins,
            observations: vec![EntryObservation::Blocked],
        };
    }
    if !page.fetched || page.status.is_none() {
        return EntryEvaluation {
            origins,
            observations: vec![EntryObservation::Unknown],
        };
    }
    let status = page.status.unwrap_or_default();
    if (300..400).contains(&status) {
        return EntryEvaluation {
            origins,
            observations: vec![EntryObservation::Redirect],
        };
    }
    if !(200..300).contains(&status) {
        return EntryEvaluation {
            origins,
            observations: vec![EntryObservation::HttpError],
        };
    }
    let mut observations = Vec::new();
    if page.noindex == Some(true) {
        observations.push(EntryObservation::Noindex);
    }
    if page
        .canonical
        .as_ref()
        .and_then(|c| c.as_deref())
        .is_some_and(|canonical| canonical != url.as_str())
    {
        observations.push(EntryObservation::Noncanonical);
    }
    if page.noindex.is_none()
        || page.canonical.is_none()
        || !origins.complete
        || origins.paths.is_empty()
    {
        observations.push(EntryObservation::Unknown);
    }
    if observations.is_empty() {
        observations.push(EntryObservation::Available);
    }
    EntryEvaluation {
        origins,
        observations,
    }
}
#[cfg(test)]
mod entry_evaluation_tests {
    use super::*;
    #[test]
    fn blocked_and_unavailable_never_become_passes() {
        let scope = Url::parse("https://example.com/").unwrap();
        let map = "https://example.com/map.xml";
        let target = "https://example.com/a";
        let doc = parse(
            map,
            b"<urlset><url><loc>https://example.com/a</loc></url></urlset>",
            1000,
            10,
        );
        let docs = std::collections::BTreeMap::from([(map.into(), doc)]);
        let mut pages = std::collections::BTreeMap::from([(
            target.into(),
            PageObservation {
                blocked: true,
                fetched: false,
                status: None,
                noindex: None,
                canonical: None,
            },
        )]);
        let e = evaluate_entry(map, target, &docs, &pages, &scope, 10);
        assert_eq!(e.observations, vec![EntryObservation::Blocked]);
        assert_eq!(e.origins.paths[0].entry_ordinal, 0);
        pages.get_mut(target).unwrap().blocked = false;
        assert_eq!(
            evaluate_entry(map, target, &docs, &pages, &scope, 10).observations,
            vec![EntryObservation::Unknown]
        );
        *pages.get_mut(target).unwrap() = PageObservation {
            blocked: false,
            fetched: true,
            status: Some(200),
            noindex: Some(true),
            canonical: Some(Some("https://example.com/b".into())),
        };
        assert_eq!(
            evaluate_entry(map, target, &docs, &pages, &scope, 10).observations,
            vec![EntryObservation::Noindex, EntryObservation::Noncanonical]
        );
        pages.get_mut(target).unwrap().status = Some(404);
        assert_eq!(
            evaluate_entry(map, target, &docs, &pages, &scope, 10).observations,
            vec![EntryObservation::HttpError]
        );
        pages.get_mut(target).unwrap().status = Some(301);
        assert_eq!(
            evaluate_entry(map, target, &docs, &pages, &scope, 10).observations,
            vec![EntryObservation::Redirect]
        );
    }
}

/// Reduce a local sitemap document to a deterministic entry prefix that fits the
/// compact UTF-8 byte budget. An incomplete parse has unknown omissions; a
/// complete document has an exact count of entries dropped by this operation.
pub fn enforce_document_budget(
    document: &mut Sitemap,
    max_bytes: usize,
) -> Result<(), &'static str> {
    loop {
        if serde_json::to_vec(document)
            .map_err(|_| "serialization_failed")?
            .len()
            <= max_bytes
        {
            return Ok(());
        }
        if document.entries.pop().is_none() {
            return Err("minimum_marker_exceeds_budget");
        }
        document.omitted_entries = document.omitted_entries.map(|n| n.saturating_add(1));
        if document.error.is_none() {
            document.error = Some("budget_limit".into());
        }
    }
}
#[cfg(test)]
mod document_budget_tests {
    use super::*;
    #[test]
    fn bounded_prefix_never_claims_full_coverage() {
        let mut doc=parse("https://example.com/map",b"<urlset><url><loc>https://example.com/a</loc></url><url><loc>https://example.com/b</loc></url></urlset>",1000,10);
        let limit = serde_json::to_vec(&doc).unwrap().len() - 1;
        enforce_document_budget(&mut doc, limit).unwrap();
        assert_eq!(doc.entries.len(), 1);
        assert_eq!(doc.omitted_entries, Some(1));
        assert_eq!(doc.error.as_deref(), Some("budget_limit"));
        assert_eq!(
            enforce_document_budget(&mut doc, 1),
            Err("minimum_marker_exceeds_budget")
        );
        let mut partial = parse("x", b"<urlset><url>", 1000, 10);
        assert_eq!(partial.omitted_entries, None);
        let _ = enforce_document_budget(&mut partial, 1);
        assert_eq!(partial.omitted_entries, None);
    }
}

/// Bounded local discovery record. Entries in index documents retain each
/// parent-hop ordinal, including repeated references to the same child.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalRun {
    pub version: u8,
    /// Coverage of retained evidence, distinct from the crawler's result.
    pub state: String,
    pub discovery_state: String,
    pub roots: Vec<String>,
    pub omitted_roots: usize,
    /// Robots sitemap directives ignored by discovery because they are invalid or out of scope.
    pub skipped_roots: usize,
    pub sources: Vec<LocalSource>,
    pub omitted_sources: usize,
    pub remaining_sources_unknown: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalSource {
    pub url: String,
    pub state: String,
    pub status: Option<u16>,
    pub body_sha256: Option<String>,
    pub document: Option<Sitemap>,
}
impl LocalRun {
    /// Compare with the actual discovery parser without changing its output.
    pub fn reconcile_consumed(&mut self, index: Option<usize>, is_index: bool, urls: &[String]) {
        let Some(doc) = index
            .and_then(|i| self.sources.get_mut(i))
            .and_then(|source| source.document.as_mut())
        else {
            return;
        };
        let retained = doc.entries.iter().filter_map(|entry| {
            entry
                .location
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
        if doc.index != is_index || !retained.eq(urls.iter().map(String::as_str)) {
            if doc.error.is_none() {
                doc.error = Some("consumed_provenance_unavailable".into());
            }
            doc.omitted_entries = None;
        }
    }
    pub fn new(roots: &[Url]) -> Self {
        let mut run = Self {
            version: 1,
            state: "in_progress".into(),
            discovery_state: "in_progress".into(),
            roots: Vec::new(),
            omitted_roots: 0,
            skipped_roots: 0,
            sources: Vec::new(),
            omitted_sources: 0,
            remaining_sources_unknown: true,
        };
        for root in roots {
            if run.roots.len() < 32 && root.as_str().len() <= 2048 {
                run.roots.push(root.to_string());
            } else {
                run.omitted_roots += 1;
            }
        }
        run
    }
    pub fn finish(&mut self, discovery_complete: bool, remaining_unknown: bool, scope: &Url) {
        self.discovery_state = if discovery_complete {
            "complete"
        } else {
            "incomplete"
        }
        .into();
        self.remaining_sources_unknown = remaining_unknown;
        let partial = !discovery_complete
            || remaining_unknown
            || self.omitted_roots > 0
            || self.skipped_roots > 0
            || self.omitted_sources > 0
            || self
                .sources
                .iter()
                .any(|source| match source.state.as_str() {
                    "skipped_duplicate" => false,
                    "fetched" => source.document.as_ref().is_none_or(|doc| {
                        doc.error.is_some()
                            || doc.omitted_entries != Some(0)
                            || doc
                                .entries
                                .iter()
                                .any(|entry| entry_scope(entry, scope) != EntryScope::InScope)
                    }),
                    _ => true,
                });
        self.state = if partial { "partial" } else { "complete" }.into();
    }
    pub fn source(
        &mut self,
        url: &Url,
        state: &str,
        status: Option<u16>,
        bytes: Option<&[u8]>,
    ) -> Option<usize> {
        if self.sources.len() >= 64 || url.as_str().len() > 2048 {
            self.omitted_sources = self.omitted_sources.saturating_add(1);
            return None;
        }
        let document = bytes.and_then(|bytes| {
            let mut doc = parse(url.as_str(), bytes, 256 * 1024, 256);
            enforce_document_budget(&mut doc, 16 * 1024)
                .ok()
                .map(|()| doc)
        });
        let state = if bytes.is_some() && document.is_none() {
            "document_budget_unknown"
        } else {
            state
        };
        self.sources.push(LocalSource {
            url: url.to_string(),
            state: state.into(),
            status,
            body_sha256: bytes.map(crate::model::digest),
            document,
        });
        Some(self.sources.len() - 1)
    }
    pub fn finish_fetch(
        &mut self,
        index: Option<usize>,
        url: &Url,
        status: Option<u16>,
        bytes: Option<&[u8]>,
    ) {
        if let Some(index) = index {
            self.sources.remove(index);
            let state = if bytes.is_some() {
                "fetched"
            } else if status.is_some() {
                "fetched_http_error"
            } else {
                "fetch_error"
            };
            self.source(url, state, status, bytes);
        }
    }
}

#[cfg(test)]
mod local_run_tests {
    use super::*;
    #[test]
    fn direct_entries_are_complete_but_nested_duplicate_is_ambiguous() {
        let source = "https://example.com/map.xml";
        let direct = parse(source, b"<urlset><url><loc>https://example.com/a</loc></url><url><loc>https://example.com/a</loc></url></urlset>", 1000, 10);
        assert_eq!(direct.error, None);
        assert_eq!(direct.omitted_entries, Some(0));
        assert_eq!(
            direct.entries.iter().map(|e| e.ordinal).collect::<Vec<_>>(),
            vec![0, 1]
        );
        let nested = parse(source, b"<urlset><url><loc>https://example.com/a</loc><image:image><image:loc>https://example.com/a</image:loc></image:image></url><url></url></urlset>", 1000, 10);
        assert_eq!(
            nested.error.as_deref(),
            Some("consumed_provenance_unavailable")
        );
        assert_eq!(nested.omitted_entries, None);
        assert_eq!(nested.entries[1].error.as_deref(), Some("missing_location"));
    }
    #[test]
    fn mismatch_preserves_existing_diagnostics_and_ordinals() {
        let url = Url::parse("https://example.com/map.xml").unwrap();
        let mut run = LocalRun::new(std::slice::from_ref(&url));
        let body = b"<urlset><url><loc>https://example.com/a</loc></url><url><loc>bad</loc></url><url><loc>https://example.com/b</loc></url><url>";
        let index = run.source(&url, "fetched", Some(200), Some(body));
        run.reconcile_consumed(index, false, &["https://example.com/other".into()]);
        let doc = run.sources[0].document.as_ref().unwrap();
        assert_eq!(doc.error.as_deref(), Some("invalid_document"));
        assert_eq!(doc.omitted_entries, None);
        assert_eq!(doc.entries[1].error.as_deref(), Some("invalid_location"));
        assert_eq!(doc.entries[2].ordinal, 2);
    }
    #[test]
    fn capture_limits_keep_unknown_omissions_and_errors() {
        let url = Url::parse("https://example.com/map.xml").unwrap();
        let mut run = LocalRun::new(std::slice::from_ref(&url));
        let body = format!(
            "<urlset>{}</urlset>",
            "<url><loc>https://example.com/a</loc></url>".repeat(300)
        );
        run.source(&url, "fetched", Some(200), Some(body.as_bytes()));
        let doc = run.sources[0].document.as_ref().unwrap();
        assert_eq!(doc.error.as_deref(), Some("entry_limit"));
        assert_eq!(doc.omitted_entries, None);
        for _ in 1..66 {
            run.source(&url, "skipped_duplicate", None, None);
        }
        assert_eq!(run.sources.len(), 64);
        assert_eq!(run.omitted_sources, 2);
        assert!(serde_json::to_vec(&run).unwrap().len() < 2 * 1024 * 1024);
    }
}
