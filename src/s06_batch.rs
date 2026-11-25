//! Bounded, local review of explicitly supplied retained responses.
//! See `docs/s06/retained-groups.md` for limits and interpretation.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::s06_detector::{self, Decision, Source};

pub const MAX_PAGES: usize = 128;
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_COMPARISONS: usize = 4096;
pub const SEMANTICS: &str = "s06-retained-groups-1";

/// Identity and HTTP metadata are caller assertions, not verified observations.
pub struct RetainedPage<'a> {
    pub identity: &'a str,
    pub source: Source<'a>,
}

#[derive(Debug, Serialize)]
pub struct Edge {
    pub left: String,
    pub right: String,
    pub left_sha256: String,
    pub right_sha256: String,
    pub left_main_sha256: String,
    pub right_main_sha256: String,
    pub similarity: f64,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Group {
    pub members: Vec<String>,
    /// Indices into `edges`; membership is transitive, not an all-pairs claim.
    pub supporting_edges: Vec<usize>,
}

#[derive(Debug, Serialize)]
pub struct PageAssessment {
    pub identity: String,
    pub soft_404_decision: &'static str,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Batch {
    pub semantics: &'static str,
    pub detector_semantics: &'static str,
    pub supplied_pages: usize,
    pub pages: Vec<PageAssessment>,
    pub comparisons: usize,
    pub omitted_comparisons: usize,
    pub unknown_reasons: BTreeMap<&'static str, usize>,
    pub no_candidate: usize,
    pub edges: Vec<Edge>,
    pub groups: Vec<Group>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BatchError {
    TooManyPages,
    TooManyBytes,
    InvalidIdentity,
    DuplicateIdentity,
}

/// Reject invalid or oversized batches before extraction or pairwise work.
///
/// # Panics
/// Panics if the detector returns a candidate without its required evidence or score.
pub fn analyze(pages: &[RetainedPage<'_>]) -> Result<Batch, BatchError> {
    if pages.len() > MAX_PAGES {
        return Err(BatchError::TooManyPages);
    }
    let mut bytes = 0usize;
    let mut ordered: Vec<_> = pages.iter().collect();
    for page in &ordered {
        if page.identity.is_empty() || page.identity.len() > MAX_ID_BYTES {
            return Err(BatchError::InvalidIdentity);
        }
        bytes = bytes.saturating_add(page.source.bytes.map_or(0, <[u8]>::len));
        if bytes > MAX_INPUT_BYTES {
            return Err(BatchError::TooManyBytes);
        }
    }
    ordered.sort_unstable_by_key(|p| p.identity);
    if ordered.windows(2).any(|p| p[0].identity == p[1].identity) {
        return Err(BatchError::DuplicateIdentity);
    }
    let total = pages.len() * pages.len().saturating_sub(1) / 2;
    let mut out = Batch {
        semantics: SEMANTICS,
        detector_semantics: s06_detector::SEMANTICS,
        supplied_pages: pages.len(),
        pages: ordered
            .iter()
            .map(|page| {
                let assessment = s06_detector::soft_404(&page.source);
                PageAssessment {
                    identity: page.identity.to_owned(),
                    soft_404_decision: match assessment.decision {
                        Decision::Candidate => "candidate",
                        Decision::NoCandidate => "no_candidate",
                        Decision::Unknown => "unknown",
                    },
                    reason: assessment.reason,
                }
            })
            .collect(),
        comparisons: 0,
        omitted_comparisons: total.saturating_sub(MAX_COMPARISONS),
        unknown_reasons: BTreeMap::new(),
        no_candidate: 0,
        edges: Vec::new(),
        groups: Vec::new(),
    };
    'pairs: for (i, left) in ordered.iter().enumerate() {
        for right in &ordered[i + 1..] {
            if out.comparisons == MAX_COMPARISONS {
                break 'pairs;
            }
            let result = s06_detector::compare(&left.source, Some(&right.source));
            out.comparisons += 1;
            match result.decision {
                Decision::Unknown => *out.unknown_reasons.entry(result.reason).or_default() += 1,
                Decision::NoCandidate => out.no_candidate += 1,
                Decision::Candidate => {
                    let a = result.evidence.expect("candidate has source evidence");
                    let b = result.peer_evidence.expect("candidate has peer evidence");
                    out.edges.push(Edge {
                        left: left.identity.to_owned(),
                        right: right.identity.to_owned(),
                        left_sha256: a.source_sha256,
                        right_sha256: b.source_sha256,
                        left_main_sha256: a.main_sha256,
                        right_main_sha256: b.main_sha256,
                        similarity: result.similarity.expect("candidate has score"),
                        reason: result.reason,
                    });
                }
            }
        }
    }
    // At most MAX_PAGES members and MAX_COMPARISONS edges are retained.
    let mut remaining: BTreeSet<usize> = (0..out.edges.len()).collect();
    while let Some(first) = remaining.pop_first() {
        let mut indices = vec![first];
        let mut members = BTreeSet::from([
            out.edges[first].left.clone(),
            out.edges[first].right.clone(),
        ]);
        loop {
            let connected: Vec<_> = remaining
                .iter()
                .copied()
                .filter(|&i| {
                    members.contains(&out.edges[i].left) || members.contains(&out.edges[i].right)
                })
                .collect();
            if connected.is_empty() {
                break;
            }
            for i in connected {
                remaining.remove(&i);
                members.insert(out.edges[i].left.clone());
                members.insert(out.edges[i].right.clone());
                indices.push(i);
            }
        }
        indices.sort_unstable();
        out.groups.push(Group {
            members: members.into_iter().collect(),
            supporting_edges: indices,
        });
    }
    Ok(out)
}
