//! Shared scaffolding for the S02-S08 section builders: the entity table, the
//! per-page capture facts, the section builder (invariants of design 3.2) and the
//! deadline.
use super::types::{
    CaptureMode, Clock, Coverage, FamilyItem, FamilyKey, FamilySection, Outcome, SectionState,
    limits_for,
};
use crate::{contract::entity_id, model::Report};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

/// Whole-build and per-family wall budgets (design section 2).
pub const TOTAL_DEADLINE: Duration = Duration::from_secs(30);
pub const FAMILY_DEADLINE: Duration = Duration::from_secs(10);
/// The analyzers' own row bound; more entities make a family partial.
pub const ANALYZER_ROWS: usize = 1000;
/// `facts` is bounded to 4,096 serialized bytes per item (invariant 6).
const FACTS_BYTES: usize = 4096;
const FACTS_STRING_BYTES: usize = 2048;
const SUMMARY_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug)]
pub struct Entity {
    pub url: String,
    pub entity_id: String,
    pub locator: String,
    pub is_resource: bool,
    pub html: bool,
    /// A 2xx response that carried a body: the only kind that can be an exact duplicate.
    pub has_ok_body: bool,
}

/// What one page's saved capture says, reduced to the small facts the builders use.
#[derive(Clone, Debug, Default)]
pub struct PageCapture {
    /// collector name -> (state, reason)
    pub collectors: BTreeMap<String, (String, Option<String>)>,
    pub soft_404: Option<(String, Option<String>)>,
    pub body_hash: Option<String>,
}

impl PageCapture {
    /// `collector_byte_limit` / `run_byte_limit` when `name` was dropped by a capture bound.
    pub fn omission(&self, name: &str) -> Option<&'static str> {
        match self.collectors.get(name) {
            Some((state, Some(reason))) if state == "unavailable" => match reason.as_str() {
                "collector_byte_limit" => Some("collector_byte_limit"),
                "run_byte_limit" => Some("run_byte_limit"),
                _ => None,
            },
            _ => None,
        }
    }
}

pub struct Ctx<'a> {
    pub entities: Vec<Entity>,
    pub by_url: HashMap<String, usize>,
    pub captures: HashMap<String, PageCapture>,
    pub clock: &'a dyn Clock,
    pub started: Instant,
    pub mode: CaptureMode,
}

impl<'a> Ctx<'a> {
    pub fn new(
        report: &Report,
        captures: HashMap<String, PageCapture>,
        clock: &'a dyn Clock,
        mode: CaptureMode,
    ) -> Self {
        let entities: Vec<Entity> = report
            .pages
            .iter()
            .enumerate()
            .map(|(n, p)| Entity {
                url: p.url.clone(),
                entity_id: entity_id(&p.url, p.is_resource),
                locator: format!("/pages/{n}"),
                is_resource: p.is_resource,
                html: p.html() && !p.is_resource,
                has_ok_body: p.status.is_some_and(|s| (200..300).contains(&s)) && p.bytes > 0,
            })
            .collect();
        let by_url = entities
            .iter()
            .enumerate()
            .map(|(n, e)| (e.url.clone(), n))
            .collect();
        Self {
            entities,
            by_url,
            captures,
            clock,
            started: clock.now(),
            mode,
        }
    }
    pub fn entity(&self, url: &str) -> Option<&Entity> {
        self.by_url.get(url).map(|n| &self.entities[*n])
    }
    pub fn total_expired(&self) -> bool {
        self.clock.now().saturating_duration_since(self.started) >= TOTAL_DEADLINE
    }
    pub fn family_expired(&self, family_started: Instant) -> bool {
        self.total_expired()
            || self.clock.now().saturating_duration_since(family_started) >= FAMILY_DEADLINE
    }
}

/// A free-text analyzer reason as a `^[a-z0-9_]{1,64}$` code.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out.truncate(64);
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "unspecified".into()
    } else {
        out
    }
}

/// Clip every string to the bound and drop what still does not fit in `facts`.
pub fn bounded_facts(facts: Map<String, Value>) -> Map<String, Value> {
    fn clip(v: Value, depth: usize) -> Value {
        match v {
            Value::String(mut s) => {
                if s.len() > FACTS_STRING_BYTES {
                    let mut end = FACTS_STRING_BYTES;
                    while !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    s.truncate(end);
                }
                Value::String(s.replace('\0', ""))
            }
            Value::Array(a) if depth < 3 => {
                Value::Array(a.into_iter().map(|x| clip(x, depth + 1)).collect())
            }
            Value::Object(o) if depth < 3 => Value::Object(
                o.into_iter()
                    .map(|(k, x)| (k, clip(x, depth + 1)))
                    .collect(),
            ),
            Value::Array(_) | Value::Object(_) => Value::Null,
            other => other,
        }
    }
    let mut out: Map<String, Value> = facts.into_iter().map(|(k, v)| (k, clip(v, 0))).collect();
    while serde_json::to_vec(&out).map_or(0, |b| b.len()) > FACTS_BYTES {
        let Some(last) = out.keys().next_back().cloned() else {
            break;
        };
        out.remove(&last);
    }
    out
}

pub struct SectionBuilder {
    key: FamilyKey,
    semantics: String,
    items: Vec<FamilyItem>,
    coverage: Coverage,
    summary: Value,
    partial_reason: Option<String>,
    deadline_omitted: u64,
}

impl SectionBuilder {
    pub fn new(key: FamilyKey, semantics: &str, ctx: &Ctx<'_>, eligible: u64) -> Self {
        let coverage = Coverage {
            entities: ctx.entities.len() as u64,
            eligible,
            ..Coverage::default()
        };
        Self {
            key,
            semantics: semantics.to_owned(),
            items: Vec::new(),
            coverage,
            summary: Value::Object(Map::new()),
            partial_reason: None,
            deadline_omitted: 0,
        }
    }
    pub fn summary(&mut self, summary: Value) {
        self.summary = summary;
    }
    pub fn partial(&mut self, reason: &str) {
        self.partial_reason.get_or_insert_with(|| reason.to_owned());
    }
    pub fn attempted(&mut self, n: u64) {
        self.coverage.attempted = n;
    }
    pub fn observed(&mut self, n: u64) {
        self.coverage.observed = n;
    }
    /// Counts an unknown without an item (the entity is unknown for `reason`).
    pub fn unknown_without_item(&mut self, reason: &str, n: u64) {
        self.coverage.unknown += n;
        *self
            .coverage
            .unknown_reasons
            .entry(reason.to_owned())
            .or_default() += n;
    }
    /// Items still to build when the deadline hits are counted, not emitted.
    pub fn deadline_hit(&mut self, remaining: u64) {
        self.deadline_omitted += remaining;
    }
    pub fn push(
        &mut self,
        entity: Option<&Entity>,
        kind: &str,
        outcome: Outcome,
        reason: Option<&str>,
        facts: Map<String, Value>,
    ) {
        let reason = reason.map(slug);
        if outcome == Outcome::Unknown {
            let r = reason.clone().unwrap_or_else(|| "unspecified".into());
            self.unknown_without_item(&r, 1);
            self.items.push(item(entity, kind, outcome, Some(r), facts));
        } else {
            self.items.push(item(entity, kind, outcome, reason, facts));
        }
    }
    pub fn finish(mut self) -> FamilySection {
        let limits = limits_for(self.key);
        self.items.sort_by(|a, b| {
            (a.outcome, &a.locator, &a.kind).cmp(&(b.outcome, &b.locator, &b.kind))
        });
        let total = self.items.len() as u64 + self.deadline_omitted;
        let mut omitted = self.deadline_omitted;
        let mut omitted_reason =
            (self.deadline_omitted > 0).then(|| "analysis_deadline".to_owned());
        if self.items.len() > limits.max_items {
            omitted += (self.items.len() - limits.max_items) as u64;
            self.items.truncate(limits.max_items);
            omitted_reason = Some("family_item_limit".into());
        }
        let truncated = omitted > 0;
        if serde_json::to_vec(&self.summary).map_or(0, |b| b.len()) > SUMMARY_BYTES {
            self.summary = serde_json::json!({"summary_omitted": "summary_byte_limit"});
        }
        let partial = truncated || self.coverage.unknown > 0 || self.partial_reason.is_some();
        FamilySection {
            section_version: 1,
            semantics: self.semantics,
            state: if partial {
                SectionState::Partial
            } else {
                SectionState::Complete
            },
            reason: self.partial_reason,
            coverage: self.coverage,
            summary: self.summary,
            items: self.items,
            items_total: total,
            omitted_items: omitted,
            omitted_reason,
            truncated,
            max_items: limits.max_items as u64,
            max_bytes: limits.max_bytes as u64,
        }
    }
}

fn item(
    entity: Option<&Entity>,
    kind: &str,
    outcome: Outcome,
    reason: Option<String>,
    facts: Map<String, Value>,
) -> FamilyItem {
    FamilyItem {
        entity_id: entity.map(|e| e.entity_id.clone()),
        locator: entity.map(|e| e.locator.clone()),
        kind: kind.to_owned(),
        outcome,
        reason,
        facts: bounded_facts(facts),
    }
}

pub fn facts(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// Unknown items for pages whose dependent collectors were dropped by a capture bound
/// (`collector_byte_limit`, `run_byte_limit`). Returns the entities that were affected,
/// so callers never also report them as `pass`.
pub fn capture_omissions(
    ctx: &Ctx<'_>,
    builder: &mut SectionBuilder,
    collectors: &[&str],
    only_html: bool,
) -> std::collections::HashSet<usize> {
    let mut affected = std::collections::HashSet::new();
    for (n, entity) in ctx.entities.iter().enumerate() {
        if only_html && !entity.html {
            continue;
        }
        let Some(capture) = ctx.captures.get(&entity.url) else {
            continue;
        };
        let dropped: Vec<(&str, &'static str)> = collectors
            .iter()
            .filter_map(|c| capture.omission(c).map(|r| (*c, r)))
            .collect();
        if let Some((collector, reason)) = dropped.first() {
            affected.insert(n);
            builder.push(
                Some(entity),
                "capture_omission",
                Outcome::Unknown,
                Some(reason),
                facts(&[("collector", Value::String((*collector).to_owned()))]),
            );
        }
    }
    affected
}

/// A section for a family that was not produced.
pub fn unavailable(key: FamilyKey, semantics: &str, reason: &str) -> FamilySection {
    FamilySection::unavailable(key, semantics, reason)
}
