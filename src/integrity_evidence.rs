//! Local-only graph analysis. Inputs must retain raw collection provenance.
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetState {
    Available,
    Blocked,
    FetchFailed,
    NotFetched,
    OutsideScope,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub url: String,
    pub state: TargetState,
    pub canonical: Option<String>,
    pub alternates: Vec<(String, String)>,
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
            TargetState::OutsideScope => break GraphState::OutsideScope,
            TargetState::FetchFailed | TargetState::NotFetched => break GraphState::Unavailable,
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
    if node.alternates.iter().any(|(_, url)| url == source) {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub source: String,
    pub ordinal: usize,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlternateEdge {
    pub path: Vec<String>,
    pub source: String,
    pub language: String,
    pub target: String,
    pub reciprocal: Reciprocity,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlternateCluster {
    pub members: Vec<String>,
    pub edges: Vec<AlternateEdge>,
    pub complete: bool,
}
/// Traverse the observed alternate graph, retaining every declaration and its origin.
/// An unavailable/out-of-scope member or an incomplete collection makes it partial.
pub fn alternate_cluster(
    start: &str,
    nodes: &BTreeMap<String, Node>,
    scope: &Url,
    limit: usize,
) -> AlternateCluster {
    let mut seen = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([(start.to_owned(), vec![start.to_owned()])]);
    let mut queued = BTreeSet::from([start.to_owned()]);
    let mut edges = Vec::new();
    let mut complete = true;
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
        for (language, target) in &node.alternates {
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
            edge_path.push(target.clone());
            edges.push(AlternateEdge {
                path: edge_path.clone(),
                source: url.clone(),
                language: language.clone(),
                target: target.clone(),
                reciprocal: reciprocity.clone(),
            });
            if reciprocity != Reciprocity::OutsideScope && queued.insert(target.clone()) {
                if queued.len() > limit || edge_path.len() > 128 {
                    complete = false;
                } else {
                    queue.push_back((target.clone(), edge_path));
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
                        ("fr".into(), b.into()),
                        ("de".into(), "https://outside.example/".into()),
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
                    alternates: vec![("en".into(), a.into())],
                    alternates_complete: true,
                },
            ),
        ]);
        let cluster = alternate_cluster(a, &nodes, &scope, 10);
        assert!(!cluster.complete);
        assert_eq!(cluster.members.len(), 2);
        assert_eq!(cluster.edges.len(), 3);
        assert_eq!(cluster.edges[0].source, a);
        assert_eq!(cluster.edges[0].reciprocal, Reciprocity::Present);
        assert_eq!(cluster.edges[1].reciprocal, Reciprocity::OutsideScope);
        assert!(!alternate_cluster(a, &nodes, &scope, 1).complete);
    }
}
