//! Local-only graph analysis. Inputs must retain raw collection provenance.
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetState {
    Available,
    Blocked,
    Redirect,
    HttpError,
    Noindex,
    FetchFailed,
    ContentUnknown,
    NotFetched,
    OutsideScope,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub url: String,
    pub state: TargetState,
    pub canonical: Option<String>,
    pub alternates: Vec<Declaration>,
    /// False when the full set of alternates could not be extracted.
    pub alternates_complete: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphResult {
    pub path: Vec<String>,
    pub state: GraphState,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphState {
    Terminal,
    Cycle,
    Blocked,
    Redirect,
    HttpError,
    Noindex,
    Unavailable,
    OutsideScope,
    Invalid,
    Limit,
}

/// Walks observed canonical links only. Unobserved and out-of-scope targets are not passes.
pub fn canonical_path(
    start: &str,
    nodes: &BTreeMap<String, Node>,
    scope: &Url,
    limit: usize,
) -> GraphResult {
    let mut path = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = start.to_owned();
    let state = loop {
        if path.len() >= limit {
            break GraphState::Limit;
        }
        let Ok(url) = Url::parse(&current) else {
            path.push(current);
            break GraphState::Invalid;
        };
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str() != scope.host_str()
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            path.push(current);
            break GraphState::OutsideScope;
        }
        path.push(current.clone());
        if !seen.insert(current.clone()) {
            break GraphState::Cycle;
        }
        let Some(node) = nodes.get(&current) else {
            break GraphState::Unavailable;
        };
        match node.state {
            TargetState::Blocked => break GraphState::Blocked,
            TargetState::Redirect => break GraphState::Redirect,
            TargetState::HttpError => break GraphState::HttpError,
            TargetState::Noindex => break GraphState::Noindex,
            TargetState::OutsideScope => break GraphState::OutsideScope,
            TargetState::FetchFailed | TargetState::ContentUnknown | TargetState::NotFetched => {
                break GraphState::Unavailable;
            }
            TargetState::Available => {}
        }
        match &node.canonical {
            Some(target) if target != &current => current.clone_from(target),
            _ => break GraphState::Terminal,
        }
    };
    GraphResult { path, state }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reciprocity {
    Present,
    Missing,
    Unknown,
    OutsideScope,
}
/// A missing return link is conclusive only when the target's extraction is complete.
pub fn reciprocal(
    source: &str,
    target: &str,
    nodes: &BTreeMap<String, Node>,
    scope: &Url,
) -> Reciprocity {
    let Ok(url) = Url::parse(target) else {
        return Reciprocity::Unknown;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str() != scope.host_str()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Reciprocity::OutsideScope;
    }
    let Some(node) = nodes.get(target) else {
        return Reciprocity::Unknown;
    };
    if node.state != TargetState::Available {
        return Reciprocity::Unknown;
    }
    if valid_alternates(node).any(|(_, _, target)| target == source) {
        Reciprocity::Present
    } else if node.alternates_complete {
        Reciprocity::Missing
    } else {
        Reciprocity::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(url: &str, canonical: Option<&str>) -> Node {
        Node {
            url: url.into(),
            state: TargetState::Available,
            canonical: canonical.map(str::to_owned),
            alternates: vec![],
            alternates_complete: true,
        }
    }
    #[test]
    fn paths_show_cycles_and_unavailable_targets() {
        let scope = Url::parse("https://example.com/").unwrap();
        let a = "https://example.com/a";
        let b = "https://example.com/b";
        let mut nodes =
            BTreeMap::from([(a.into(), node(a, Some(b))), (b.into(), node(b, Some(a)))]);
        assert_eq!(
            canonical_path(a, &nodes, &scope, 10),
            GraphResult {
                path: vec![a.into(), b.into(), a.into()],
                state: GraphState::Cycle
            }
        );
        nodes.get_mut(b).unwrap().state = TargetState::Blocked;
        assert_eq!(
            canonical_path(a, &nodes, &scope, 10).state,
            GraphState::Blocked
        );
        nodes.get_mut(b).unwrap().state = TargetState::Redirect;
        assert_eq!(
            canonical_path(a, &nodes, &scope, 10).state,
            GraphState::Redirect
        );
        nodes.get_mut(b).unwrap().state = TargetState::HttpError;
        assert_eq!(
            canonical_path(a, &nodes, &scope, 10).state,
            GraphState::HttpError
        );
        nodes.get_mut(b).unwrap().state = TargetState::Noindex;
        assert_eq!(
            canonical_path(a, &nodes, &scope, 10).state,
            GraphState::Noindex
        );
        nodes.get_mut(b).unwrap().state = TargetState::FetchFailed;
        assert_eq!(
            canonical_path(a, &nodes, &scope, 10).state,
            GraphState::Unavailable
        );
        nodes.get_mut(a).unwrap().canonical = Some("https://other.example/".into());
        assert_eq!(
            canonical_path(a, &nodes, &scope, 10).state,
            GraphState::OutsideScope
        );
    }
    #[test]
    fn reciprocity_requires_complete_target() {
        let scope = Url::parse("https://example.com/").unwrap();
        let mut target = node("https://example.com/b", None);
        target.alternates_complete = false;
        let mut nodes = BTreeMap::from([(target.url.clone(), target)]);
        assert_eq!(
            reciprocal(
                "https://example.com/a",
                "https://example.com/b",
                &nodes,
                &scope
            ),
            Reciprocity::Unknown
        );
        nodes
            .get_mut("https://example.com/b")
            .unwrap()
            .alternates_complete = true;
        assert_eq!(
            reciprocal(
                "https://example.com/a",
                "https://example.com/b",
                &nodes,
                &scope
            ),
            Reciprocity::Missing
        );
    }
}

fn split_header(value: &str, delimiter: char) -> Vec<&str> {
    let (mut quoted, mut angle, mut escaped, mut start) = (false, false, false, 0);
    let mut parts = Vec::new();
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
        }
        if !quoted {
            if c == '<' {
                angle = true;
            }
            if c == '>' {
                angle = false;
            }
        }
        if c == delimiter && !quoted && !angle {
            parts.push(&value[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&value[start..]);
    parts
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub source: String,
    pub ordinal: usize,
    pub entry_index: Option<usize>,
    pub raw: Option<String>,
    pub language: Option<String>,
    pub state: DeclarationState,
    pub target: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclarationState {
    Valid,
    Empty,
    Missing,
    Invalid,
    OutsideScope,
    OutsideHead,
}
/// Interpret retained HTML link occurrences using a separately observed effective base.
/// Out-of-head and outside-scope values are retained and never treated as valid targets.
pub fn declarations(
    collection: &crate::collection_evidence::Collection,
    base: &Url,
    scope: &Url,
    canonical: bool,
) -> Vec<Declaration> {
    collection
        .occurrences
        .iter()
        .filter(|o| {
            o.source.ends_with("_link")
                && if canonical {
                    o.raw_rel.as_deref().is_some_and(|r| {
                        r.split_whitespace()
                            .any(|v| v.eq_ignore_ascii_case("canonical"))
                    })
                } else {
                    o.raw_language.is_some()
                        && o.raw_rel.as_deref().is_some_and(|r| {
                            r.split_whitespace()
                                .any(|v| v.eq_ignore_ascii_case("alternate"))
                        })
                }
        })
        .map(|o| {
            let target = o
                .raw_value
                .as_deref()
                .filter(|v| !v.trim().is_empty())
                .and_then(|v| crate::model::normalize(base, v))
                .map(|u| u.to_string());
            let state = if o.raw_value.is_none() {
                DeclarationState::Missing
            } else if o.raw_value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                DeclarationState::Empty
            } else if target.is_none() {
                DeclarationState::Invalid
            } else if !o.source.starts_with("html_head_") {
                DeclarationState::OutsideHead
            } else if target
                .as_deref()
                .and_then(|u| Url::parse(u).ok())
                .is_none_or(|u| u.host_str() != scope.host_str() || u.port().is_some())
            {
                DeclarationState::OutsideScope
            } else {
                DeclarationState::Valid
            };
            Declaration {
                source: o.source.clone(),
                ordinal: o.ordinal,
                entry_index: None,
                raw: o.raw_value.clone(),
                language: o.raw_language.clone(),
                state,
                target,
            }
        })
        .collect()
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    #[test]
    fn malformed_empty_and_outside_scope_are_not_valid() {
        let base = Url::parse("https://example.com/").unwrap();
        let c = crate::collection_evidence::html_occurrences(
            "<head><link rel='canonical' href=''><link rel='canonical' href='https://[bad'><link rel='canonical' href='https://elsewhere.example/'><link rel='canonical' href='/ok'></head>",
            true,
        );
        let d = declarations(&c, &base, &base, true);
        assert_eq!(
            d.iter().map(|d| d.state.clone()).collect::<Vec<_>>(),
            vec![
                DeclarationState::Empty,
                DeclarationState::Invalid,
                DeclarationState::OutsideScope,
                DeclarationState::Valid
            ]
        );
    }
}

fn valid_alternates(node: &Node) -> impl Iterator<Item = (&Declaration, &str, &str)> {
    node.alternates.iter().filter_map(|d| {
        if d.state != DeclarationState::Valid {
            return None;
        }
        Some((d, d.language.as_deref()?, d.target.as_deref()?))
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlternateEdge {
    pub path: Vec<String>,
    pub source: String,
    pub language: String,
    pub target: String,
    pub reciprocal: Reciprocity,
    pub declaration: Declaration,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlternateCluster {
    pub members: Vec<String>,
    pub edges: Vec<AlternateEdge>,
    pub complete: bool,
}
/// Traverse the observed alternate graph in both directions, retaining each declaration.
/// Paths can follow incoming edges; source and target preserve declaration direction.
/// `universe_complete` must cover discovery and alternate extraction for every relevant
/// page. An unavailable/out-of-scope member or incomplete collection makes it partial.
pub fn alternate_cluster(
    start: &str,
    nodes: &BTreeMap<String, Node>,
    scope: &Url,
    limit: usize,
    universe_complete: bool,
) -> AlternateCluster {
    let in_scope = Url::parse(start).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str() == scope.host_str()
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
    });
    if !in_scope {
        return AlternateCluster {
            members: vec![start.into()],
            edges: Vec::new(),
            complete: false,
        };
    }
    let mut seen = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([(start.to_owned(), vec![start.to_owned()])]);
    let mut queued = BTreeSet::from([start.to_owned()]);
    let mut edges = Vec::new();
    let mut complete = universe_complete;
    let mut reverse = BTreeMap::<String, Vec<String>>::new();
    let mut indexed = 0usize;
    'index: for (source, node) in nodes {
        if node.state != TargetState::Available {
            continue;
        }
        for (_, _, target) in valid_alternates(node) {
            if indexed >= limit.saturating_mul(20).min(100_000) {
                complete = false;
                break 'index;
            }
            reverse
                .entry(target.to_owned())
                .or_default()
                .push(source.clone());
            indexed += 1;
        }
    }
    while let Some((url, path)) = queue.pop_front() {
        if seen.contains(&url) {
            continue;
        }
        if seen.len() >= limit {
            complete = false;
            break;
        }
        seen.insert(url.clone());
        let Some(node) = nodes.get(&url) else {
            complete = false;
            continue;
        };
        if node.state != TargetState::Available || !node.alternates_complete {
            complete = false;
        }
        if node.state != TargetState::Available {
            continue;
        }
        if let Some(incoming) = reverse.get(&url) {
            for source in incoming {
                let in_scope = Url::parse(source).is_ok_and(|u| {
                    matches!(u.scheme(), "http" | "https")
                        && u.host_str() == scope.host_str()
                        && u.port().is_none()
                        && u.username().is_empty()
                        && u.password().is_none()
                });
                if !in_scope {
                    complete = false;
                    continue;
                }
                if queued.insert(source.clone()) {
                    let mut source_path = path.clone();
                    source_path.push(source.clone());
                    if queued.len() > limit || source_path.len() > 128 {
                        complete = false;
                    } else {
                        queue.push_back((source.clone(), source_path));
                    }
                }
            }
        }
        for (declaration, language, target) in valid_alternates(node) {
            if edges.len() >= limit.saturating_mul(20).min(100_000) {
                complete = false;
                break;
            }
            let reciprocity = reciprocal(&url, target, nodes, scope);
            if matches!(
                reciprocity,
                Reciprocity::Unknown | Reciprocity::OutsideScope
            ) {
                complete = false;
            }
            let mut edge_path = path.clone();
            edge_path.push(target.to_owned());
            edges.push(AlternateEdge {
                path: edge_path.clone(),
                source: url.clone(),
                language: language.to_owned(),
                target: target.to_owned(),
                reciprocal: reciprocity.clone(),
                declaration: declaration.clone(),
            });
            if reciprocity != Reciprocity::OutsideScope && queued.insert(target.to_owned()) {
                if queued.len() > limit || edge_path.len() > 128 {
                    complete = false;
                } else {
                    queue.push_back((target.to_owned(), edge_path));
                }
            }
        }
    }
    AlternateCluster {
        members: seen.into_iter().collect(),
        edges,
        complete,
    }
}
#[cfg(test)]
fn test_alternate(language: &str, target: &str) -> Declaration {
    Declaration {
        source: "html_head_link".into(),
        ordinal: 0,
        entry_index: None,
        raw: Some(target.into()),
        language: Some(language.into()),
        state: DeclarationState::Valid,
        target: Some(target.into()),
    }
}
#[cfg(test)]
mod cluster_tests {
    use super::*;
    #[test]
    fn preserves_origin_and_partial_targets() {
        let scope = Url::parse("https://example.com/").unwrap();
        let a = "https://example.com/a";
        let b = "https://example.com/b";
        let nodes = BTreeMap::from([
            (
                a.into(),
                Node {
                    url: a.into(),
                    state: TargetState::Available,
                    canonical: None,
                    alternates: vec![
                        test_alternate("fr", b),
                        test_alternate("de", "https://outside.example/"),
                    ],
                    alternates_complete: true,
                },
            ),
            (
                b.into(),
                Node {
                    url: b.into(),
                    state: TargetState::Available,
                    canonical: None,
                    alternates: vec![test_alternate("en", a)],
                    alternates_complete: true,
                },
            ),
        ]);
        let cluster = alternate_cluster(a, &nodes, &scope, 10, true);
        assert!(!cluster.complete);
        assert_eq!(cluster.members.len(), 2);
        assert_eq!(cluster.edges.len(), 3);
        assert_eq!(cluster.edges[0].source, a);
        assert_eq!(cluster.edges[0].reciprocal, Reciprocity::Present);
        assert_eq!(cluster.edges[1].reciprocal, Reciprocity::OutsideScope);
        assert!(!alternate_cluster(a, &nodes, &scope, 1, true).complete);
        let mut incoming_only = nodes.clone();
        incoming_only.get_mut(a).unwrap().alternates.clear();
        let inbound = alternate_cluster(a, &incoming_only, &scope, 10, true);
        assert_eq!(inbound.members.len(), 2);
        assert_eq!(inbound.edges[0].source, b);
        assert_eq!(inbound.edges[0].reciprocal, Reciprocity::Missing);
        assert!(!alternate_cluster(a, &incoming_only, &scope, 10, false).complete);
        assert!(!alternate_cluster("https://outside.example/", &nodes, &scope, 10, true).complete);
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    #[test]
    fn paired_graph_fixture() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/s02-s03/integrity-cases.json")).unwrap();
        let scope = Url::parse("https://example.com/").unwrap();
        for case in cases.as_array().unwrap() {
            let nodes = case["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    let url = row[0].as_str().unwrap();
                    let canonical = row[1].as_str().unwrap();
                    (
                        url.to_owned(),
                        Node {
                            url: url.into(),
                            state: TargetState::Available,
                            canonical: Some(canonical.into()),
                            alternates: vec![],
                            alternates_complete: true,
                        },
                    )
                })
                .collect();
            let graph = canonical_path(case["start"].as_str().unwrap(), &nodes, &scope, 10);
            let expected = match case["state"].as_str().unwrap() {
                "cycle" => GraphState::Cycle,
                "outside_scope" => GraphState::OutsideScope,
                "unavailable" => GraphState::Unavailable,
                _ => panic!("bad fixture"),
            };
            assert_eq!(graph.state, expected, "{}", case["name"]);
            assert_eq!(
                graph.path,
                case["path"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            );
        }
    }
}

fn balanced_link_header(raw: &str) -> bool {
    let (mut quoted, mut angle, mut escaped) = (false, false, false);
    for ch in raw.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' && !angle {
            quoted = !quoted;
        }
        if !quoted && ch == '<' {
            if angle {
                return false;
            }
            angle = true;
        }
        if !quoted && ch == '>' {
            if !angle {
                return false;
            }
            angle = false;
        }
    }
    !quoted && !angle && !escaped
}

/// Interpret Link header entries while retaining the enclosing field and entry ordinal.
/// Malformed entries make the collection incomplete instead of proving absence.
pub fn header_declarations(
    collection: &crate::collection_evidence::Collection,
    base: &Url,
    scope: &Url,
    canonical: bool,
) -> (Vec<Declaration>, bool) {
    let mut declarations = Vec::new();
    let mut complete = collection.complete;
    for field in collection.occurrences.iter().filter(|o| o.source == "link") {
        let Some(raw) = &field.raw_value else {
            complete = false;
            continue;
        };
        if !balanced_link_header(raw) {
            complete = false;
        }
        for (entry_index, entry) in split_header(raw, ',').into_iter().enumerate() {
            let Some((target, attributes)) = entry
                .trim()
                .strip_prefix('<')
                .and_then(|v| v.split_once('>'))
            else {
                complete = false;
                continue;
            };
            let mut rel = None;
            let mut language = None;
            for attr in split_header(attributes, ';') {
                if let Some((key, value)) = attr.trim().split_once('=') {
                    let value = value.trim().trim_matches('"');
                    if key.trim().eq_ignore_ascii_case("rel") && rel.replace(value).is_some() {
                        complete = false;
                    }
                    if key.trim().eq_ignore_ascii_case("hreflang")
                        && language.replace(value).is_some()
                    {
                        complete = false;
                    }
                }
            }
            let wanted = if canonical {
                rel.is_some_and(|r| {
                    r.split_whitespace()
                        .any(|v| v.eq_ignore_ascii_case("canonical"))
                })
            } else {
                rel.is_some_and(|r| {
                    r.split_whitespace()
                        .any(|v| v.eq_ignore_ascii_case("alternate"))
                }) && language.is_some()
            };
            if !wanted {
                continue;
            }
            let resolved = (!target.trim().is_empty())
                .then(|| crate::model::normalize(base, target))
                .flatten();
            let state = if target.trim().is_empty() {
                DeclarationState::Empty
            } else if resolved.is_none() {
                DeclarationState::Invalid
            } else if resolved
                .as_ref()
                .is_some_and(|u| u.host_str() != scope.host_str() || u.port().is_some())
            {
                DeclarationState::OutsideScope
            } else {
                DeclarationState::Valid
            };
            declarations.push(Declaration {
                source: "http_link".into(),
                ordinal: field.ordinal,
                entry_index: Some(entry_index),
                raw: Some(target.into()),
                language: language.map(str::to_owned),
                state,
                target: resolved.map(Into::into),
            });
        }
    }
    (declarations, complete)
}
#[cfg(test)]
mod header_tests {
    use super::*;
    #[test]
    fn repeated_fields_and_malformed_entries_retain_boundaries() {
        let c = crate::collection_evidence::http_occurrences(&[
            ("link", "</a>; rel=canonical, </b>; rel=canonical"),
            ("link", "broken"),
        ]);
        let base = Url::parse("https://example.com/").unwrap();
        let (items, complete) = header_declarations(&c, &base, &base, true);
        assert!(!complete);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].ordinal, 0);
        assert_eq!(items[1].entry_index, Some(1));
        let malformed =
            crate::collection_evidence::http_occurrences(&[("link", "</a>; rel=\"canonical")]);
        assert!(!header_declarations(&malformed, &base, &base, true).1);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalAgreement {
    Consistent,
    Conflicting,
    Absent,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalAssessment {
    pub declarations: Vec<Declaration>,
    pub agreement: CanonicalAgreement,
}
/// Compare all collected HTML and HTTP declarations; unavailable sources or invalid
/// occurrences cannot produce an absence or consistency conclusion.
pub fn canonical_assessment(
    capture: &crate::collection_evidence::PageCapture,
    base: &Url,
    scope: &Url,
) -> CanonicalAssessment {
    let mut declarations_found = Vec::new();
    let mut complete = capture.state == "available";
    if let Some(html) = &capture.html {
        complete &= html.complete;
        declarations_found.extend(declarations(html, base, scope, true));
    } else {
        complete = false;
    }
    if let Some(http) = &capture.http {
        let (headers, header_complete) = header_declarations(http, base, scope, true);
        complete &= header_complete;
        declarations_found.extend(headers);
    } else {
        complete = false;
    }
    let valid_targets: BTreeSet<_> = declarations_found
        .iter()
        .filter(|d| d.state == DeclarationState::Valid)
        .filter_map(|d| d.target.clone())
        .collect();
    let has_invalid = declarations_found
        .iter()
        .any(|d| d.state != DeclarationState::Valid);
    let agreement = if valid_targets.len() > 1 {
        CanonicalAgreement::Conflicting
    } else if !complete || has_invalid {
        CanonicalAgreement::Unknown
    } else if valid_targets.is_empty() {
        CanonicalAgreement::Absent
    } else {
        CanonicalAgreement::Consistent
    };
    CanonicalAssessment {
        declarations: declarations_found,
        agreement,
    }
}
#[cfg(test)]
mod canonical_assessment_tests {
    use super::*;
    #[test]
    fn source_conflicts_and_missing_evidence_are_distinct() {
        let base = Url::parse("https://example.com/").unwrap();
        let mut capture =
            crate::collection_evidence::PageCapture::unavailable(base.as_str(), "available");
        capture.html = Some(crate::collection_evidence::html_occurrences(
            "<head><link rel='canonical' href='/a'></head>",
            true,
        ));
        capture.http = Some(crate::collection_evidence::http_occurrences(&[(
            "link",
            "</b>; rel=canonical",
        )]));
        let result = canonical_assessment(&capture, &base, &base);
        assert_eq!(result.agreement, CanonicalAgreement::Conflicting);
        assert_eq!(result.declarations.len(), 2);
        capture.http = None;
        assert_eq!(
            canonical_assessment(&capture, &base, &base).agreement,
            CanonicalAgreement::Unknown
        );
        capture.html = Some(crate::collection_evidence::html_occurrences(
            "<head></head>",
            true,
        ));
        capture.http = Some(crate::collection_evidence::http_occurrences(&[]));
        assert_eq!(
            canonical_assessment(&capture, &base, &base).agreement,
            CanonicalAgreement::Absent
        );
    }
}

/// Construct an analyzable node only when canonical source evidence is complete.
/// Blocked/failed/uncollected pages retain their state without inferring directives.
pub fn node_from_capture(
    capture: &crate::collection_evidence::PageCapture,
    base: &Url,
    scope: &Url,
) -> Option<Node> {
    if !Url::parse(&capture.url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str() == scope.host_str()
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
    }) {
        return None;
    }
    let state = match capture.state.as_str() {
        "available" => TargetState::Available,
        "robots_blocked" => TargetState::Blocked,
        "fetch_failed" => TargetState::FetchFailed,
        _ => TargetState::NotFetched,
    };
    if state != TargetState::Available {
        return Some(Node {
            url: capture.url.clone(),
            state,
            canonical: None,
            alternates: vec![],
            alternates_complete: false,
        });
    }
    let assessment = canonical_assessment(capture, base, scope);
    let canonical = match assessment.agreement {
        CanonicalAgreement::Consistent => assessment
            .declarations
            .iter()
            .find(|d| d.state == DeclarationState::Valid)
            .and_then(|d| d.target.clone()),
        CanonicalAgreement::Absent => None,
        CanonicalAgreement::Conflicting | CanonicalAgreement::Unknown => return None,
    };
    let html = capture.html.as_ref()?;
    let http = capture.http.as_ref()?;
    let mut alternate_declarations = declarations(html, base, scope, false);
    let (header_alternates, header_complete) = header_declarations(http, base, scope, false);
    alternate_declarations.extend(header_alternates);
    let complete = html.complete
        && header_complete
        && alternate_declarations
            .iter()
            .all(|d| d.state == DeclarationState::Valid);
    let alternates = alternate_declarations;
    Some(Node {
        url: capture.url.clone(),
        state,
        canonical,
        alternates,
        alternates_complete: complete,
    })
}
#[cfg(test)]
mod capture_node_tests {
    use super::*;
    #[test]
    fn mixed_declarations_keep_locators_but_only_valid_alternates_form_edges() {
        let base = Url::parse("https://example.com/a").unwrap();
        let mut capture =
            crate::collection_evidence::PageCapture::unavailable(base.as_str(), "available");
        capture.html = Some(crate::collection_evidence::html_occurrences(
            "<html><head><link rel='stylesheet' hreflang='fr' href='/style.css'><link rel='alternate' hreflang='fr' href='/b'><link rel='alternate' hreflang='es' href='https://outside.example/es'></head><body><link rel='alternate' hreflang='de' href='/bad'></body></html>",
            true,
        ));
        capture.http = Some(crate::collection_evidence::http_occurrences(&[
            (
                "link",
                "</b>; rel=alternate; hreflang=fr, </b>; rel=alternate; hreflang=fr",
            ),
            ("link", "</b>; rel=alternate; hreflang=fr"),
        ]));
        let node = node_from_capture(&capture, &base, &base).unwrap();
        assert_eq!(node.alternates.len(), 6);
        assert!(!node.alternates_complete);
        assert!(
            node.alternates
                .iter()
                .any(|d| d.state == DeclarationState::OutsideScope)
        );
        assert_eq!(
            node.alternates
                .iter()
                .filter(|d| d.state == DeclarationState::OutsideHead)
                .count(),
            1
        );
        assert!(
            node.alternates
                .iter()
                .all(|d| d.target.as_deref() != Some("https://example.com/style.css"))
        );
        let nodes = BTreeMap::from([(node.url.clone(), node)]);
        let cluster = alternate_cluster(base.as_str(), &nodes, &base, 20, true);
        assert_eq!(cluster.edges.len(), 4);
        assert!(
            cluster
                .edges
                .iter()
                .all(|edge| edge.target == "https://example.com/b"
                    && edge.declaration.state == DeclarationState::Valid)
        );
        let locators: Vec<_> = cluster
            .edges
            .iter()
            .map(|e| {
                (
                    e.declaration.source.as_str(),
                    e.declaration.ordinal,
                    e.declaration.entry_index,
                )
            })
            .collect();
        assert!(
            locators
                .iter()
                .any(|(source, _, index)| *source == "html_head_link" && index.is_none())
        );
        assert!(locators.contains(&("http_link", 0, Some(0))));
        assert!(locators.contains(&("http_link", 0, Some(1))));
        assert!(locators.contains(&("http_link", 1, Some(0))));
        assert_eq!(
            reciprocal("https://example.com/bad", base.as_str(), &nodes, &base),
            Reciprocity::Unknown
        );
    }
    #[test]
    fn incomplete_or_conflicting_canonical_cannot_be_a_terminal_node() {
        let base = Url::parse("https://example.com/a").unwrap();
        let mut c =
            crate::collection_evidence::PageCapture::unavailable(base.as_str(), "available");
        c.html = Some(crate::collection_evidence::html_occurrences(
            "<head><link rel='canonical' href='/a'></head>",
            true,
        ));
        c.http = Some(crate::collection_evidence::http_occurrences(&[(
            "link",
            "</b>; rel=canonical",
        )]));
        assert!(node_from_capture(&c, &base, &base).is_none());
        c.http = Some(crate::collection_evidence::http_occurrences(&[]));
        assert_eq!(
            node_from_capture(&c, &base, &base)
                .unwrap()
                .canonical
                .as_deref(),
            Some("https://example.com/a")
        );
        c.html.as_mut().unwrap().complete = false;
        assert!(node_from_capture(&c, &base, &base).is_none());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlternateIssue {
    MissingSelf,
    InvalidLanguage {
        language: String,
        target: String,
    },
    ConflictingLanguage {
        language: String,
        targets: Vec<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlternateAssessment {
    pub issues: Vec<AlternateIssue>,
    pub complete: bool,
}
/// Evaluate the fully observed declarations of one node; missing x-default is
/// not treated as a defect. An incomplete collection never proves absence.
pub fn assess_alternates(node: &Node) -> AlternateAssessment {
    let complete = node.state == TargetState::Available && node.alternates_complete;
    let mut issues = Vec::new();
    let mut by_language = BTreeMap::<String, BTreeSet<String>>::new();
    for (_, language, target) in valid_alternates(node) {
        let normalized = language.to_ascii_lowercase();
        if normalized != "x-default" && language.parse::<language_tags::LanguageTag>().is_err() {
            issues.push(AlternateIssue::InvalidLanguage {
                language: language.to_owned(),
                target: target.to_owned(),
            });
        }
        by_language
            .entry(normalized)
            .or_default()
            .insert(target.to_owned());
    }
    for (language, targets) in by_language {
        if targets.len() > 1 {
            issues.push(AlternateIssue::ConflictingLanguage {
                language,
                targets: targets.into_iter().collect(),
            });
        }
    }
    if complete
        && valid_alternates(node).next().is_some()
        && !valid_alternates(node).any(|(_, _, target)| target == node.url)
    {
        issues.push(AlternateIssue::MissingSelf);
    }
    AlternateAssessment { issues, complete }
}
#[cfg(test)]
mod alternate_assessment_tests {
    use super::*;
    #[test]
    fn missing_self_requires_complete_collection_and_xdefault_is_optional() {
        let mut n = Node {
            url: "https://example.com/a".into(),
            state: TargetState::Available,
            canonical: None,
            alternates: vec![
                test_alternate("en", "https://example.com/b"),
                test_alternate("EN", "https://example.com/c"),
            ],
            alternates_complete: false,
        };
        let partial = assess_alternates(&n);
        assert!(!partial.complete);
        assert!(!partial.issues.contains(&AlternateIssue::MissingSelf));
        n.alternates_complete = true;
        let complete = assess_alternates(&n);
        assert!(complete.issues.contains(&AlternateIssue::MissingSelf));
        assert!(
            complete
                .issues
                .iter()
                .any(|i| matches!(i, AlternateIssue::ConflictingLanguage { .. }))
        );
        n.alternates = vec![test_alternate("en", &n.url)];
        assert_eq!(assess_alternates(&n).issues.len(), 0);
    }
}

/// Attach observed status and a bot-specific indexing decision to an available
/// graph node. Unknown indexing cannot become an ordinary terminal pass.
pub fn apply_target_observation(
    node: &mut Node,
    status: Option<u16>,
    index: &crate::index_evidence::Decision,
) {
    if node.state != TargetState::Available {
        return;
    }
    node.state = match status {
        None => TargetState::NotFetched,
        Some(code) if (300..400).contains(&code) => TargetState::Redirect,
        Some(code) if !(200..300).contains(&code) => TargetState::HttpError,
        Some(_) => match index {
            crate::index_evidence::Decision::Restricted => TargetState::Noindex,
            crate::index_evidence::Decision::Unknown => TargetState::ContentUnknown,
            crate::index_evidence::Decision::Allowed => TargetState::Available,
        },
    };
}
#[cfg(test)]
mod target_observation_tests {
    use super::*;
    #[test]
    fn unknown_indexing_cannot_become_terminal() {
        let mut node = Node {
            url: "https://example.com/a".into(),
            state: TargetState::Available,
            canonical: None,
            alternates: vec![],
            alternates_complete: true,
        };
        apply_target_observation(
            &mut node,
            Some(200),
            &crate::index_evidence::Decision::Unknown,
        );
        assert_eq!(node.state, TargetState::ContentUnknown);
    }
}
