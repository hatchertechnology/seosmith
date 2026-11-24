//! Read-only, user-facing S02/S03 evidence from terminal local checkpoints.
//!
//! The readers reuse the merged robots, indexing, integrity, sitemap and graph
//! analyzers. They never migrate, recover, fetch, deliver or rewrite a database.

use anyhow::{Context, Result, ensure};
use futures_util::TryStreamExt;
use seosmith::{
    collection_evidence::PageCapture,
    graph_evidence::{self, GraphCoverageInput, LocalPageEvidence},
    index_evidence::{self, Decision, Intent},
    integrity_evidence::{
        self, AlternateAssessment, AlternateIssue, CanonicalAgreement, CanonicalAssessment,
        Declaration, DeclarationState, GraphResult, GraphState, Node, TargetState,
    },
    model,
    sitemap_evidence::{self, EntryOrigins, LocalRun, Sitemap},
};
use serde_json::{Value, json};
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use url::Url;

const MAX_ROWS: usize = 1000;
const MAX_EVIDENCE: i64 = 64 * 1024;
const MAX_PAGE_JSON: i64 = 512 * 1024;
const MAX_INPUT: usize = 2 * 1024 * 1024;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_OMISSIONS: usize = 1000;
const MAX_FINDINGS: usize = 2000;
const MAX_SITEMAP_LOOKUPS: usize = 2000;
const MAX_CANONICAL_DEPTH: usize = 32;
const MAX_ALTERNATE_DEPTH: usize = 32;
const MAX_ALTERNATE_CLUSTERS: usize = 500;
const BOTS: [&str; 5] = [
    "Seosmith",
    "Googlebot",
    "Bingbot",
    "GPTBot",
    "OAI-SearchBot",
];

#[derive(Clone, Default)]
struct PageFacts {
    status: Option<u16>,
    blocked: bool,
    error: Option<String>,
    content_type: String,
    depth: usize,
    in_sitemap: bool,
    resource: bool,
}

struct PageReadout {
    url: String,
    facts: PageFacts,
    evidence: Value,
    capture: Option<PageCapture>,
    indexing: Vec<index_evidence::EffectiveIndexing>,
    seosmith_index: Decision,
    canonical: Option<CanonicalAssessment>,
    alternates: Option<AlternateAssessment>,
    alternate_node: Option<Node>,
    sitemap: Value,
}

enum CaptureBinding {
    Bound,
    RunLimit,
    Unbound,
}

fn capture_url_matches(capture: &Value, url: &str) -> CaptureBinding {
    let bound = |value: &str| value == url;
    let digest_bound = || {
        capture["url"].is_null()
            && capture["url_omission"] == "url_copy_byte_limit"
            && capture["url_sha256"].as_str() == Some(model::digest(url.as_bytes()).as_str())
    };
    if capture["url"].as_str().is_some_and(bound) {
        CaptureBinding::Bound
    } else if capture["url"].is_null()
        && capture["url_omission"] == "run_byte_limit"
        && capture["url_sha256"].as_str() == Some(model::digest(url.as_bytes()).as_str())
    {
        CaptureBinding::RunLimit
    } else if digest_bound() {
        CaptureBinding::Bound
    } else {
        CaptureBinding::Unbound
    }
}

fn facts_from_page(page: Option<&Value>) -> PageFacts {
    let Some(page) = page else {
        return PageFacts::default();
    };
    PageFacts {
        status: page["status"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok()),
        blocked: page["blocked_by_robots"].as_bool().unwrap_or(false),
        error: page["error"]
            .as_str()
            .filter(|value| value.len() <= 256)
            .map(str::to_owned),
        content_type: page["content_type"]
            .as_str()
            .filter(|value| value.len() <= 256)
            .unwrap_or_default()
            .to_owned(),
        depth: page["depth"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or_default(),
        in_sitemap: page["in_sitemap"].as_bool().unwrap_or(false),
        resource: page["is_resource"].as_bool().unwrap_or(false),
    }
}

fn capture_state_for(facts: &PageFacts) -> &'static str {
    if facts.blocked {
        return "robots_blocked";
    }
    if facts.status.is_none() {
        return "fetch_failed";
    }
    let content_type = facts
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if facts
        .status
        .is_some_and(|status| (200..300).contains(&status))
        && matches!(
            content_type.as_str(),
            "text/html" | "application/xhtml+xml" | "application/vnd.wap.xhtml+xml"
        )
    {
        "available"
    } else {
        "content_unavailable"
    }
}

fn collector_data<'a>(capture: &'a Value, name: &str) -> Option<&'a Value> {
    let collectors: Vec<_> = capture["collectors"]
        .as_array()?
        .iter()
        .filter(|collector| collector["collector"] == name)
        .collect();
    if collectors.len() != 1 {
        return None;
    }
    collectors[0].get("data").filter(|value| !value.is_null())
}

fn capture_from_value(value: &Value, url: &str, fallback_state: &str) -> PageCapture {
    let state = value["state"].as_str().unwrap_or(fallback_state).to_owned();
    let robots = collector_data(value, "robots")
        .and_then(|data| data.get("decisions"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .or_else(|| {
            value
                .get("robots")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        })
        .unwrap_or_default();
    let robots_source = collector_data(value, "robots")
        .and_then(|data| data["source"].as_str())
        .or_else(|| value["robots_source"].as_str())
        .unwrap_or("unavailable")
        .to_owned();
    let http = collector_data(value, "http_occurrences")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .or_else(|| {
            value
                .get("http")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        });
    let html = collector_data(value, "html_occurrences")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .or_else(|| {
            value
                .get("html")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        });
    PageCapture {
        version: "s02-s03-local-1".into(),
        url: url.into(),
        state,
        robots,
        robots_source,
        http,
        html,
    }
}

fn collector_summary(capture: &Value) -> Vec<Value> {
    let mut names = BTreeMap::<String, usize>::new();
    let mut result = Vec::new();
    for collector in capture["collectors"].as_array().into_iter().flatten() {
        let name = collector["collector"].as_str().unwrap_or("unknown");
        let count = names.entry(name.into()).or_default();
        *count += 1;
        result.push(json!({
            "collector": name,
            "version": collector["version"].as_u64(),
            "state": collector["state"].as_str(),
            "reason": collector["reason"].as_str().filter(|value| value.len() <= 256),
            "duplicate": *count > 1,
        }));
    }
    result
}

fn graph_from_capture(capture: &Value, url: &str) -> Result<LocalPageEvidence, &'static str> {
    let collectors = capture["collectors"]
        .as_array()
        .ok_or("collectors_absent")?;
    let graph: Vec<_> = collectors
        .iter()
        .filter(|collector| collector["collector"] == "graph")
        .collect();
    if graph.len() != 1 {
        return Err(if graph.is_empty() {
            "graph_collector_absent"
        } else {
            "duplicate_graph_collector"
        });
    }
    let graph = graph[0];
    if graph["version"].as_u64() != Some(1)
        || !matches!(graph["state"].as_str(), Some("observed" | "partial"))
    {
        return Err("graph_unavailable_or_unsupported");
    }
    let mut item = serde_json::from_value::<LocalPageEvidence>(graph["data"].clone())
        .map_err(|_| "invalid_graph_data")?;
    if item.url != url {
        return Err("unbound_graph_url");
    }
    if graph["state"] == "partial" {
        item.extraction_complete = false;
    }
    Ok(item)
}

fn alternates_for_capture(
    capture: &PageCapture,
    base: &Url,
    scope: &Url,
) -> (Vec<Declaration>, bool) {
    let mut complete = capture.state == "available";
    let mut declarations = Vec::new();
    if let Some(html) = &capture.html {
        complete &= html.complete;
        declarations.extend(integrity_evidence::declarations(html, base, scope, false));
    } else {
        complete = false;
    }
    if let Some(http) = &capture.http {
        let (headers, header_complete) =
            integrity_evidence::header_declarations(http, base, scope, false);
        complete &= header_complete;
        declarations.extend(headers);
    } else {
        complete = false;
    }
    complete &= declarations
        .iter()
        .all(|declaration| declaration.state == DeclarationState::Valid);
    (declarations, complete)
}

fn agreement_name(value: &CanonicalAgreement) -> &'static str {
    match value {
        CanonicalAgreement::Consistent => "consistent",
        CanonicalAgreement::Conflicting => "conflicting",
        CanonicalAgreement::Absent => "absent",
        CanonicalAgreement::Unknown => "unknown",
    }
}

fn decision_name(value: &Decision) -> &'static str {
    match value {
        Decision::Restricted => "restricted",
        Decision::Allowed => "allowed",
        Decision::Unknown => "unknown",
    }
}

fn crawl_name(value: &index_evidence::CrawlPermission) -> &'static str {
    match value {
        index_evidence::CrawlPermission::Allowed => "allowed",
        index_evidence::CrawlPermission::Denied => "denied",
        index_evidence::CrawlPermission::Unknown => "unknown",
    }
}

fn intent_name(value: &index_evidence::IntentResult) -> &'static str {
    match value {
        index_evidence::IntentResult::Conforms => "conforms",
        index_evidence::IntentResult::Conflicts => "conflicts",
        index_evidence::IntentResult::Unknown => "unknown",
    }
}

fn content_name(value: index_evidence::ContentState) -> &'static str {
    match value {
        index_evidence::ContentState::Available => "available",
        index_evidence::ContentState::Blocked => "blocked",
        index_evidence::ContentState::FetchFailed => "fetch_failed",
        index_evidence::ContentState::Truncated => "truncated",
    }
}

fn declaration_state_name(value: &DeclarationState) -> &'static str {
    match value {
        DeclarationState::Valid => "valid",
        DeclarationState::Empty => "empty",
        DeclarationState::Missing => "missing",
        DeclarationState::Invalid => "invalid",
        DeclarationState::OutsideScope => "outside_scope",
        DeclarationState::OutsideHead => "outside_head",
    }
}

fn graph_state_name(value: &GraphState) -> &'static str {
    match value {
        GraphState::Terminal => "terminal",
        GraphState::Cycle => "cycle",
        GraphState::Blocked => "blocked",
        GraphState::Redirect => "redirect",
        GraphState::HttpError => "http_error",
        GraphState::Noindex => "noindex",
        GraphState::Unavailable => "unavailable",
        GraphState::OutsideScope => "outside_scope",
        GraphState::Invalid => "invalid",
        GraphState::Limit => "limit",
    }
}

fn declaration_json(value: &Declaration) -> Value {
    json!({
        "source": value.source,
        "ordinal": value.ordinal,
        "entry_index": value.entry_index,
        "raw": value.raw,
        "language": value.language,
        "state": declaration_state_name(&value.state),
        "target": value.target,
    })
}

fn canonical_json(value: &CanonicalAssessment) -> Value {
    let target = value
        .declarations
        .iter()
        .find(|declaration| declaration.state == DeclarationState::Valid)
        .and_then(|declaration| declaration.target.clone());
    json!({
        "state": agreement_name(&value.agreement),
        "target": target,
        "declarations": value.declarations.iter().map(declaration_json).collect::<Vec<_>>(),
    })
}

fn alternate_issue_json(value: &AlternateIssue) -> Value {
    match value {
        AlternateIssue::MissingSelf => json!({"issue":"missing_self"}),
        AlternateIssue::InvalidLanguage { language, target } => json!({
            "issue":"invalid_language", "language":language, "target":target
        }),
        AlternateIssue::ConflictingLanguage { language, targets } => json!({
            "issue":"conflicting_language", "language":language, "targets":targets
        }),
    }
}

fn alternates_json(node: &Node, assessment: &AlternateAssessment) -> Value {
    json!({
        "state": if assessment.complete {"retained_complete"} else {"partial"},
        "retained_complete": assessment.complete,
        "discovery_complete": false,
        "issues": assessment.issues.iter().map(alternate_issue_json).collect::<Vec<_>>(),
        "declarations": node.alternates.iter().map(declaration_json).collect::<Vec<_>>(),
    })
}

fn effective_json(value: &index_evidence::EffectiveIndexing) -> Value {
    let directives = &value.directives;
    json!({
        "bot": value.bot,
        "crawl": crawl_name(&value.crawl),
        "robots": value.robots,
        "content_state": content_name(directives.content_state),
        "index": decision_name(&directives.index),
        "follow": decision_name(&directives.follow),
        "intent": intent_name(&value.intent),
        "desired_policy": "unknown",
        "conflicts": directives.conflicts,
        "restrictions": directives.restrictions,
        "applicable_sources": directives.applicable.iter().map(|source| json!({
            "source": source.source,
            "bot": source.bot,
            "value": source.value,
            "locator": source.locator,
        })).collect::<Vec<_>>(),
    })
}

fn path_json(value: &GraphResult) -> Value {
    json!({
        "path": value.path,
        "state": graph_state_name(&value.state),
    })
}

fn key_url(scope: &Url, value: &str) -> String {
    model::normalize(scope, value).map_or_else(|| value.trim().to_owned(), |url| url.to_string())
}

struct SitemapContext {
    run: Option<LocalRun>,
    documents: BTreeMap<String, Sitemap>,
    targets: BTreeSet<String>,
    state: &'static str,
    reason: Option<String>,
}

impl SitemapContext {
    fn from_evidence(raw: Option<String>, oversized: bool, scope: &Url) -> Self {
        if oversized {
            return Self {
                run: None,
                documents: BTreeMap::new(),
                targets: BTreeSet::new(),
                state: "unavailable",
                reason: Some("sitemap_evidence_byte_limit".into()),
            };
        }
        let Some(raw) = raw else {
            return Self {
                run: None,
                documents: BTreeMap::new(),
                targets: BTreeSet::new(),
                state: "unavailable",
                reason: Some("sitemap_evidence_absent_or_legacy".into()),
            };
        };
        let Ok(run) = serde_json::from_str::<LocalRun>(&raw) else {
            return Self {
                run: None,
                documents: BTreeMap::new(),
                targets: BTreeSet::new(),
                state: "invalid",
                reason: Some("unsupported_sitemap_evidence".into()),
            };
        };
        if run.version != 1 {
            return Self {
                run: None,
                documents: BTreeMap::new(),
                targets: BTreeSet::new(),
                state: "invalid",
                reason: Some("unsupported_sitemap_evidence_version".into()),
            };
        }
        let documents = run
            .sources
            .iter()
            .filter_map(|source| {
                source
                    .document
                    .clone()
                    .map(|document| (source.url.clone(), document))
            })
            .collect::<BTreeMap<_, _>>();
        let targets = documents
            .values()
            .flat_map(|document| document.entries.iter())
            .filter_map(|entry| entry.location.as_deref())
            .map(|location| key_url(scope, location))
            .collect();
        Self {
            run: Some(run),
            documents,
            targets,
            state: "available",
            reason: None,
        }
    }
}

fn lastmod_json(
    documents: &BTreeMap<String, Sitemap>,
    documents_path: &[String],
    ordinal: usize,
) -> Option<Value> {
    let leaf = documents_path.last()?;
    let entry = documents
        .get(leaf)?
        .entries
        .iter()
        .find(|entry| entry.ordinal == ordinal)?;
    Some(json!({
        "value": entry.lastmod,
        "state": entry.lastmod_state,
        "source": entry.sitemap,
        "ordinal": entry.ordinal,
    }))
}

fn origins_json(
    root: &str,
    origins: &EntryOrigins,
    documents: &BTreeMap<String, Sitemap>,
    scope: &Url,
) -> Value {
    let paths = origins
        .paths
        .iter()
        .map(|path| {
            json!({
                "documents": path.documents,
                "index_entry_ordinals": path.index_entry_ordinals,
                "entry_ordinal": path.entry_ordinal,
                "location": path.location,
                "lastmod": lastmod_json(documents, &path.documents, path.entry_ordinal),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "root": root,
        "complete": origins.complete,
        "paths": paths,
        "state": if origins.complete {"observed"} else {"partial"},
        "target_in_scope": origins.paths.first().is_some_and(|path| {
            Url::parse(&path.location).is_ok_and(|url| url.host_str() == scope.host_str())
        }),
    })
}

fn page_sitemap(
    context: &SitemapContext,
    scope: &Url,
    page_url: &str,
    in_sitemap: bool,
    lookups: &mut usize,
    omitted: &mut usize,
) -> Value {
    let Some(run) = &context.run else {
        return json!({
            "state": context.state,
            "reason": context.reason,
            "complete": false,
        });
    };
    let target = key_url(scope, page_url);
    if !in_sitemap && !context.targets.contains(&target) {
        return json!({
            "state": if run.state == "complete" {"not_listed"} else {"unknown"},
            "reason": "not_in_retained_sitemaps",
            "complete": run.state == "complete",
            "target": target,
            "run_state": run.state,
            "discovery_state": run.discovery_state,
        });
    }
    if run.roots.is_empty() {
        return json!({
            "state": "unknown",
            "reason": "no_retained_sitemap_roots",
            "complete": false,
            "target": target,
            "run_state": run.state,
        });
    }
    let mut roots = Vec::new();
    let mut complete = true;
    let mut budget_exhausted = false;
    for root in &run.roots {
        if *lookups >= MAX_SITEMAP_LOOKUPS {
            budget_exhausted = true;
            complete = false;
            break;
        }
        *lookups += 1;
        let origins = sitemap_evidence::entry_origins(
            root,
            &target,
            &context.documents,
            scope,
            MAX_ALTERNATE_DEPTH,
        );
        complete &= origins.complete;
        roots.push(origins_json(root, &origins, &context.documents, scope));
    }
    if roots
        .iter()
        .all(|root| root["paths"].as_array().is_none_or(Vec::is_empty))
    {
        complete = false;
    }
    if budget_exhausted {
        *omitted = omitted.saturating_add(1);
    }
    json!({
        "state": if complete {"observed"} else {"partial"},
        "reason": if budget_exhausted {
            Some("origin_lookup_budget")
        } else if roots.iter().all(|root| root["paths"].as_array().is_none_or(Vec::is_empty)) {
            Some("target_not_found_in_retained_documents")
        } else {
            None
        },
        "complete": complete && run.state == "complete" && !run.remaining_sources_unknown,
        "target": target,
        "run_state": run.state,
        "discovery_state": run.discovery_state,
        "remaining_sources_unknown": run.remaining_sources_unknown,
        "roots": roots,
    })
}

fn sitemap_summary(context: &SitemapContext) -> Value {
    let Some(run) = &context.run else {
        return json!({"state": context.state, "reason": context.reason});
    };
    let sources = run
        .sources
        .iter()
        .map(|source| {
            json!({
                "url": source.url,
                "state": source.state,
                "status": source.status,
                "body_sha256": source.body_sha256,
                "document": source.document.as_ref().map(|document| json!({
                    "index": document.index,
                    "entries": document.entries.len(),
                    "omitted_entries": document.omitted_entries,
                    "error": document.error,
                })),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "state": run.state,
        "discovery_state": run.discovery_state,
        "roots": run.roots,
        "omitted_roots": run.omitted_roots,
        "skipped_roots": run.skipped_roots,
        "omitted_sources": run.omitted_sources,
        "remaining_sources_unknown": run.remaining_sources_unknown,
        "sources": sources,
        "freshness": {
            "state": "lastmod_syntax_only",
            "reason": "sitemap lastmod is not a measured observation time",
        },
    })
}

fn robots_summary(raw: Option<String>, oversized: bool) -> Value {
    if oversized {
        return json!({"state":"unavailable","reason":"robots_run_byte_limit"});
    }
    let Some(raw) = raw else {
        return json!({"state":"unavailable","reason":"robots_run_absent_or_legacy"});
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return json!({"state":"invalid","reason":"unsupported_robots_evidence"});
    };
    if value["version"].as_u64() != Some(1) {
        return json!({"state":"invalid","reason":"unsupported_robots_evidence_version"});
    }
    json!({
        "state":"observed",
        "origins":value["origins"],
        "omitted_origins":value["omitted_origins"],
    })
}

fn manual_node(url: &str, facts: &PageFacts) -> Node {
    let state = if facts.blocked {
        TargetState::Blocked
    } else {
        match facts.status {
            None if facts.error.is_some() => TargetState::FetchFailed,
            None => TargetState::NotFetched,
            Some(code) if (300..400).contains(&code) => TargetState::Redirect,
            Some(code) if !(200..300).contains(&code) => TargetState::HttpError,
            Some(_) => TargetState::ContentUnknown,
        }
    };
    Node {
        url: url.into(),
        state,
        canonical: None,
        alternates: vec![],
        alternates_complete: false,
    }
}

fn push_omission(omitted: &mut Vec<Value>, value: Value) {
    if omitted.len() < MAX_OMISSIONS {
        omitted.push(value);
    }
}

fn push_finding(findings: &mut Vec<Value>, omitted: &mut usize, value: Value) {
    if findings.len() < MAX_FINDINGS {
        findings.push(value);
    } else {
        *omitted = omitted.saturating_add(1);
    }
}

fn source_for(declaration: &Declaration) -> Value {
    json!({
        "collector": declaration.source,
        "ordinal": declaration.ordinal,
        "entry_index": declaration.entry_index,
        "state": declaration_state_name(&declaration.state),
        "raw": declaration.raw,
    })
}

fn build_findings(
    pages: &[PageReadout],
    paths: &BTreeMap<String, GraphResult>,
    clusters: &[integrity_evidence::AlternateCluster],
) -> (Vec<Value>, usize) {
    let mut findings = Vec::new();
    let mut omitted = 0usize;
    for page in pages {
        let path = paths.get(&page.url);
        if page.facts.blocked {
            push_finding(
                &mut findings,
                &mut omitted,
                json!({
                    "rule":"page_blocked_by_robots", "kind":"negative", "url":page.url,
                    "path":[page.url], "source":{"collector":"saved_page","signal":"blocked_by_robots"},
                    "reason":"crawler admission blocked this page; content directives are not inferred",
                }),
            );
        }
        if page
            .facts
            .status
            .is_some_and(|status| (300..400).contains(&status))
        {
            push_finding(
                &mut findings,
                &mut omitted,
                json!({
                    "rule":"page_redirect", "kind":"negative", "url":page.url,
                    "path":[page.url], "source":{"collector":"saved_page","signal":"status"},
                    "reason":"observed redirect status",
                }),
            );
        } else if page
            .facts
            .status
            .is_some_and(|status| !(200..300).contains(&status))
        {
            push_finding(
                &mut findings,
                &mut omitted,
                json!({
                    "rule":"page_http_error", "kind":"negative", "url":page.url,
                    "path":[page.url], "source":{"collector":"saved_page","signal":"status"},
                    "reason":"observed unsuccessful HTTP status",
                }),
            );
        }
        if page.capture.is_none() {
            push_finding(
                &mut findings,
                &mut omitted,
                json!({
                    "rule":"page_evidence_unavailable", "kind":"uncertain", "url":page.url,
                    "path":[page.url], "source":{"collector":"page_checkpoint"},
                    "reason":page.evidence["reason"],
                }),
            );
        }
        for effective in &page.indexing {
            let source = json!({
                "collector":"robots_and_directives",
                "robots_source": page.capture.as_ref().map(|capture| capture.robots_source.clone()),
                "locators": effective.directives.applicable.iter().map(|source| source.locator.clone()).collect::<Vec<_>>(),
            });
            if effective.crawl == index_evidence::CrawlPermission::Denied {
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"crawl_denied", "kind":"negative", "url":page.url, "bot":effective.bot,
                        "path":[page.url], "source":source,
                        "reason":effective.robots.as_ref().and_then(|robots| robots.rule.clone()),
                    }),
                );
            } else if effective.crawl == index_evidence::CrawlPermission::Unknown {
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"crawl_unknown", "kind":"uncertain", "url":page.url, "bot":effective.bot,
                        "path":[page.url], "source":source,
                        "reason":"robots evidence is absent, unavailable or unusable",
                    }),
                );
            }
            if !effective.directives.conflicts.is_empty() {
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"directive_conflict", "kind":"uncertain", "url":page.url, "bot":effective.bot,
                        "path":[page.url], "source":source,
                        "reason":effective.directives.conflicts,
                    }),
                );
            }
            match effective.directives.index {
                Decision::Restricted => push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"index_restricted", "kind":"negative", "url":page.url, "bot":effective.bot,
                        "path":[page.url], "source":source,
                        "reason":effective.directives.restrictions,
                    }),
                ),
                Decision::Unknown => push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"index_unknown", "kind":"uncertain", "url":page.url, "bot":effective.bot,
                        "path":[page.url], "source":source,
                        "reason":content_name(effective.directives.content_state),
                    }),
                ),
                Decision::Allowed if !effective.directives.applicable.is_empty() => {
                    push_finding(
                        &mut findings,
                        &mut omitted,
                        json!({
                            "rule":"index_allowed", "kind":"positive", "url":page.url, "bot":effective.bot,
                            "path":[page.url], "source":source,
                            "reason":"observed directives permit indexing",
                        }),
                    );
                }
                Decision::Allowed => {}
            }
        }
        if let Some(canonical) = &page.canonical {
            let source = json!({
                "collector":"canonical_declarations",
                "declarations":canonical.declarations.iter().map(source_for).collect::<Vec<_>>(),
            });
            for declaration in &canonical.declarations {
                if declaration.state != DeclarationState::Valid {
                    push_finding(
                        &mut findings,
                        &mut omitted,
                        json!({
                            "rule":"canonical_declaration_issue", "kind": if declaration.state == DeclarationState::Invalid {"negative"} else {"uncertain"},
                            "url":page.url, "path":[page.url, declaration.target.clone().unwrap_or_default()],
                            "source":source_for(declaration), "reason":declaration_state_name(&declaration.state),
                        }),
                    );
                }
            }
            match canonical.agreement {
                CanonicalAgreement::Conflicting => push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"canonical_conflicting", "kind":"negative", "url":page.url,
                        "path":[page.url], "source":source, "reason":"multiple valid canonical targets",
                    }),
                ),
                CanonicalAgreement::Unknown => push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"canonical_unknown", "kind":"uncertain", "url":page.url,
                        "path":[page.url], "source":source, "reason":"source or declaration evidence is incomplete",
                    }),
                ),
                CanonicalAgreement::Absent => push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"canonical_absent", "kind":"positive", "url":page.url,
                        "path":[page.url], "source":source, "reason":"complete sources retained no canonical declaration",
                    }),
                ),
                CanonicalAgreement::Consistent => {}
            }
        }
        if let Some(path) = path {
            let kind = match path.state {
                GraphState::Terminal => "positive",
                GraphState::Unavailable => "uncertain",
                _ => "negative",
            };
            push_finding(
                &mut findings,
                &mut omitted,
                json!({
                    "rule":format!("canonical_path_{}", graph_state_name(&path.state)),
                    "kind":kind, "url":page.url, "path":path.path,
                    "source":{"collector":"canonical_path","url":page.url},
                    "reason":if path.state == GraphState::Terminal {"observed terminal canonical path"} else {"canonical path did not reach an ordinary terminal"},
                }),
            );
        }
        if let (Some(node), Some(assessment)) = (&page.alternate_node, &page.alternates) {
            let source = json!({
                "collector":"alternate_declarations",
                "declarations":node.alternates.iter().map(source_for).collect::<Vec<_>>(),
            });
            for issue in &assessment.issues {
                let (rule, kind, path, reason) = match issue {
                    AlternateIssue::MissingSelf => (
                        "hreflang_missing_self",
                        "negative",
                        vec![page.url.clone()],
                        "complete retained alternate collection does not declare this page".into(),
                    ),
                    AlternateIssue::InvalidLanguage { language, target } => (
                        "hreflang_invalid_language",
                        "negative",
                        vec![page.url.clone(), target.clone()],
                        format!("invalid language tag: {language}"),
                    ),
                    AlternateIssue::ConflictingLanguage { language, targets } => (
                        "hreflang_conflicting_language",
                        "negative",
                        targets.clone(),
                        format!("multiple targets for language: {language}"),
                    ),
                };
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":rule, "kind":kind, "url":page.url, "path":path,
                        "source":source, "reason":reason,
                    }),
                );
            }
            if assessment.complete && assessment.issues.is_empty() && !node.alternates.is_empty() {
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"hreflang_consistent", "kind":"positive", "url":page.url,
                        "path":[page.url], "source":source,
                        "reason":"retained alternate collection is internally consistent",
                    }),
                );
            }
        }
        if let Some(roots) = page.sitemap["roots"].as_array() {
            let mut found = false;
            for root in roots {
                if let Some(paths) = root["paths"].as_array() {
                    for path in paths {
                        found = true;
                        let mut finding_path = path["documents"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        finding_path.push(page.url.clone());
                        push_finding(
                            &mut findings,
                            &mut omitted,
                            json!({
                                "rule":"sitemap_origin", "kind":"positive", "url":page.url,
                                "path":finding_path,
                                "source":{
                                    "collector":"sitemap_evidence",
                                    "root":root["root"],
                                    "documents":path["documents"],
                                    "index_entry_ordinals":path["index_entry_ordinals"],
                                    "entry_ordinal":path["entry_ordinal"],
                                    "lastmod":path["lastmod"],
                                },
                                "reason":"retained sitemap traversal explains this entry",
                            }),
                        );
                    }
                }
            }
            if !found && page.facts.in_sitemap {
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"sitemap_origin_unavailable", "kind":"uncertain", "url":page.url,
                        "path":[page.url], "source":{"collector":"sitemap_evidence","roots":roots.iter().map(|root| root["root"].clone()).collect::<Vec<_>>()},
                        "reason":page.sitemap["reason"],
                    }),
                );
            }
        }
    }
    for cluster in clusters {
        let complete = cluster.complete;
        for edge in &cluster.edges {
            let path = edge.path.clone();
            push_finding(
                &mut findings,
                &mut omitted,
                json!({
                    "rule":"hreflang_edge", "kind":"positive", "url":edge.source,
                    "path":path, "source":source_for(&edge.declaration),
                    "reason":{"language":edge.language,"target":edge.target},
                }),
            );
            if edge.reciprocal == integrity_evidence::Reciprocity::Missing {
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"hreflang_reciprocity_missing", "kind":"negative", "url":edge.source,
                        "path":[edge.source.clone(), edge.target.clone()],
                        "source":source_for(&edge.declaration),
                        "reason":"target has a complete retained alternate collection without a return declaration",
                    }),
                );
            } else if edge.reciprocal == integrity_evidence::Reciprocity::Unknown {
                push_finding(
                    &mut findings,
                    &mut omitted,
                    json!({
                        "rule":"hreflang_reciprocity_unknown", "kind":"uncertain", "url":edge.source,
                        "path":[edge.source.clone(), edge.target.clone()],
                        "source":source_for(&edge.declaration),
                        "reason":"target alternate evidence is unavailable or incomplete",
                    }),
                );
            }
        }
        if !complete {
            push_finding(
                &mut findings,
                &mut omitted,
                json!({
                    "rule":"hreflang_cluster_partial", "kind":"uncertain",
                "url":cluster.members.first().cloned().unwrap_or_default(),
                    "path":cluster.members, "source":{"collector":"alternate_cluster"},
                    "reason":"retained alternate membership is partial or discovery is not proven complete",
                }),
            );
        }
    }
    (findings, omitted)
}

async fn table_exists(pool: &sqlx::SqlitePool, name: &str) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
    )
    .bind(name)
    .fetch_one(pool)
    .await?
        > 0)
}

async fn read_optional_evidence(
    pool: &sqlx::SqlitePool,
    table: &str,
    id: &str,
) -> Result<(Option<String>, bool)> {
    if !table_exists(pool, table).await? {
        return Ok((None, false));
    }
    let row = match table {
        "local_sitemap_run" => {
            sqlx::query(
                "SELECT CASE WHEN length(CAST(evidence AS BLOB)) <= ? THEN evidence END AS evidence, \
                 length(CAST(evidence AS BLOB)) > ? AS oversized FROM local_sitemap_run WHERE job_id=?",
            )
            .bind(MAX_EVIDENCE)
            .bind(MAX_EVIDENCE)
            .bind(id)
            .fetch_optional(pool)
            .await?
        }
        "local_robots_run" => {
            sqlx::query(
                "SELECT CASE WHEN length(CAST(evidence AS BLOB)) <= ? THEN evidence END AS evidence, \
                 length(CAST(evidence AS BLOB)) > ? AS oversized FROM local_robots_run WHERE job_id=?",
            )
            .bind(MAX_EVIDENCE)
            .bind(MAX_EVIDENCE)
            .bind(id)
            .fetch_optional(pool)
            .await?
        }
        _ => None,
    };
    let Some(row) = row else {
        return Ok((None, false));
    };
    Ok((row.try_get("evidence")?, row.try_get("oversized")?))
}

pub async fn run(path: &Path, id: &str) -> Result<()> {
    ensure!(path.is_file(), "database does not exist");
    // Immutable mode never creates SQLite sidecars. Refuse WAL even if empty:
    // ignoring a live or uncheckpointed WAL could silently analyze stale data.
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    ensure!(
        !Path::new(&wal).exists(),
        "database has a WAL; close writers and checkpoint it before analysis"
    );
    let mut journal = path.as_os_str().to_os_string();
    journal.push("-journal");
    ensure!(
        !Path::new(&journal).exists(),
        "database has a rollback journal; use a quiesced snapshot"
    );
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .context("open existing database read-only")?;

    let columns = sqlx::query("PRAGMA table_info(jobs)")
        .fetch_all(&pool)
        .await?;
    let has_finalization = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "local_finalization");
    let job_query = if has_finalization {
        "SELECT CASE WHEN length(CAST(request AS BLOB)) <= 65536 THEN request END AS request, \
         state,execution_mode,local_finalization,CASE WHEN length(CAST(started_at AS BLOB)) <= 128 THEN started_at END AS started_at, \
         created,report IS NOT NULL AS report_present,report_hash,delivery_state FROM jobs WHERE id=?"
    } else {
        "SELECT CASE WHEN length(CAST(request AS BLOB)) <= 65536 THEN request END AS request, \
         state,execution_mode,NULL AS local_finalization,CASE WHEN length(CAST(started_at AS BLOB)) <= 128 THEN started_at END AS started_at, \
         created,report IS NOT NULL AS report_present,report_hash,delivery_state FROM jobs WHERE id=?"
    };
    let job = sqlx::query(job_query)
        .bind(id)
        .fetch_optional(&pool)
        .await
        .context("unsupported or legacy jobs schema")?
        .context("job absent")?;
    let state: String = job.try_get("state")?;
    let mode: Option<String> = job.try_get("execution_mode")?;
    ensure!(
        state == "local_only" && mode.as_deref() == Some("local"),
        "job is not terminal local-only (legacy, active, or unsupported)"
    );
    let finalization: Option<String> = job.try_get("local_finalization")?;
    let completion = match finalization.as_deref() {
        Some("completed") => "completed",
        Some("interrupted") => "interrupted",
        _ => "unknown",
    };
    let request: Option<String> = job.try_get("request")?;
    let request = request.context("saved request absent or exceeds 64 KiB")?;
    let request: Value = serde_json::from_str(&request).context("unsupported saved request")?;
    let seed = request
        .get("url")
        .and_then(Value::as_str)
        .context("saved seed absent")?
        .to_owned();
    let seed_url = Url::parse(&seed).context("saved seed is not a URL")?;
    ensure!(
        matches!(seed_url.scheme(), "http" | "https")
            && seed_url.host_str().is_some()
            && seed_url.port().is_none()
            && seed_url.username().is_empty()
            && seed_url.password().is_none(),
        "saved seed is outside supported scope"
    );
    let started_at: Option<String> = job.try_get("started_at")?;
    let created: i64 = job.try_get("created")?;
    let report_present: bool = job.try_get("report_present")?;
    let report_hash: Option<String> = job.try_get("report_hash")?;
    let delivery_state: String = job.try_get("delivery_state")?;

    let (sitemap_raw, sitemap_oversized) =
        read_optional_evidence(&pool, "local_sitemap_run", id).await?;
    let (robots_raw, robots_oversized) =
        read_optional_evidence(&pool, "local_robots_run", id).await?;
    let sitemap = SitemapContext::from_evidence(sitemap_raw, sitemap_oversized, &seed_url);
    let robots = robots_summary(robots_raw, robots_oversized);

    let mut rows = sqlx::query(
        "SELECT CASE WHEN length(CAST(f.url AS BLOB)) <= 8192 THEN f.url END AS url, \
         f.depth,f.in_sitemap,f.resource, \
         CASE WHEN length(CAST(f.page AS BLOB)) <= ? THEN f.page END AS page, \
         f.page IS NOT NULL AS has_page, \
         CASE WHEN length(CAST(e.evidence AS BLOB)) <= ? THEN e.evidence END AS evidence, \
         e.evidence IS NOT NULL AS has_evidence \
         FROM frontier f LEFT JOIN local_page_evidence e ON e.job_id=f.job_id AND e.url=f.url \
         WHERE f.job_id=? ORDER BY f.url LIMIT ?",
    )
    .bind(MAX_PAGE_JSON)
    .bind(MAX_EVIDENCE)
    .bind(id)
    .bind(i64::try_from(MAX_ROWS + 1)?)
    .fetch(&pool);

    let mut pages = Vec::new();
    let mut omitted = Vec::new();
    let mut row_count = 0usize;
    let mut row_limit = false;
    let mut input_bytes = 0usize;
    let mut sitemap_lookups = 0usize;
    let mut sitemap_omissions = 0usize;
    let mut navigation_omitted = Vec::new();
    let mut navigation_pages = Vec::new();
    let mut navigation_bytes = 0usize;

    while let Some(row) = rows
        .try_next()
        .await
        .context("unsupported or legacy checkpoint schema")?
    {
        if row_count == MAX_ROWS {
            row_limit = true;
            break;
        }
        row_count += 1;
        let url: Option<String> = row.try_get("url")?;
        let Some(url) = url else {
            push_omission(&mut omitted, json!({"url":null,"reason":"url_byte_limit"}));
            continue;
        };
        let has_page: bool = row.try_get("has_page")?;
        let page_raw: Option<String> = row.try_get("page")?;
        let page_value = page_raw
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
        let mut facts = facts_from_page(page_value.as_ref());
        facts.depth = row
            .try_get::<i64, _>("depth")?
            .try_into()
            .unwrap_or_default();
        facts.in_sitemap = row.try_get("in_sitemap")?;
        facts.resource = row.try_get("resource")?;
        if !has_page {
            facts.status = None;
        }
        let page_reason = if !has_page {
            Some("page_checkpoint_absent")
        } else if page_value.is_none() {
            Some("page_json_invalid_or_byte_limit")
        } else {
            None
        };

        let has_evidence: bool = row.try_get("has_evidence")?;
        let evidence_raw: Option<String> = row.try_get("evidence")?;
        let mut evidence = json!({
            "state":"unavailable",
            "reason":if has_evidence {"evidence_byte_limit"} else {"evidence_absent_or_legacy"},
            "capture_state":Value::Null,
            "collectors":[],
        });
        let mut capture = None;
        let mut capture_envelope = None;
        if has_evidence {
            if let Some(raw) = evidence_raw {
                if input_bytes.saturating_add(raw.len()) > MAX_INPUT {
                    evidence["reason"] = json!("aggregate_input_limit");
                    push_omission(
                        &mut omitted,
                        json!({"url":url,"reason":"aggregate_input_limit"}),
                    );
                } else {
                    input_bytes += raw.len();
                    match serde_json::from_str::<Value>(&raw) {
                        Ok(envelope) => {
                            let capture_value = &envelope["capture"];
                            if capture_value["version"].as_u64() == Some(1) {
                                match capture_url_matches(capture_value, &url) {
                                    CaptureBinding::RunLimit => {
                                        evidence["reason"] = json!("run_byte_limit");
                                        push_omission(
                                            &mut omitted,
                                            json!({"url":url,"reason":"run_byte_limit"}),
                                        );
                                    }
                                    CaptureBinding::Unbound => {
                                        evidence["reason"] =
                                            json!("unsupported_or_unbound_capture");
                                        push_omission(
                                            &mut omitted,
                                            json!({"url":url,"reason":"unsupported_or_unbound_capture"}),
                                        );
                                    }
                                    CaptureBinding::Bound => {
                                        let parsed = capture_from_value(
                                            capture_value,
                                            &url,
                                            capture_state_for(&facts),
                                        );
                                        evidence = json!({
                                            "state":"present",
                                            "reason":Value::Null,
                                            "capture_state":parsed.state,
                                            "collectors":collector_summary(capture_value),
                                        });
                                        capture_envelope = Some(capture_value.clone());
                                        capture = Some(parsed);
                                    }
                                }
                            } else {
                                evidence["reason"] = json!("unsupported_capture_version");
                            }
                        }
                        Err(_) => {
                            evidence["reason"] = json!("invalid_evidence_envelope");
                        }
                    }
                }
            } else {
                push_omission(
                    &mut omitted,
                    json!({"url":url,"reason":"evidence_byte_limit"}),
                );
            }
        } else {
            push_omission(
                &mut omitted,
                json!({"url":url,"reason":"evidence_absent_or_legacy"}),
            );
        }
        let index_capture = capture.clone().unwrap_or_else(|| {
            PageCapture::unavailable(
                &url,
                if facts.blocked {
                    "robots_blocked"
                } else if facts.status.is_none() {
                    "fetch_failed"
                } else {
                    "content_unavailable"
                },
            )
        });
        let indexing = BOTS
            .iter()
            .map(|bot| {
                index_evidence::effective_indexing(bot, &index_capture, &Intent::Unspecified)
            })
            .collect::<Vec<_>>();
        let seosmith_index = indexing[0].directives.index.clone();
        let base = Url::parse(&url).ok();
        let canonical = capture.as_ref().and_then(|capture| {
            base.as_ref()
                .map(|base| integrity_evidence::canonical_assessment(capture, base, &seed_url))
        });
        let alternate = capture.as_ref().and_then(|capture| {
            let base = base.as_ref()?;
            let (declarations, complete) = alternates_for_capture(capture, base, &seed_url);
            let node = Node {
                url: url.clone(),
                state: TargetState::Available,
                canonical: None,
                alternates: declarations,
                alternates_complete: complete,
            };
            let assessment = integrity_evidence::assess_alternates(&node);
            Some((assessment, node))
        });
        let graph = capture_envelope
            .as_ref()
            .and_then(|capture| graph_from_capture(capture, &url).ok());
        if capture.is_some() && graph.is_none() {
            push_omission(
                &mut omitted,
                json!({"url":url,"reason":"graph_collector_absent_or_invalid"}),
            );
        }
        if let Some(item) = &graph {
            if navigation_bytes.saturating_add(
                item.url.len().saturating_add(512).saturating_add(
                    item.links
                        .iter()
                        .map(|link| {
                            link.raw_href
                                .len()
                                .saturating_add(link.fetch_url.len())
                                .saturating_add(link.accessible_name.len())
                                .saturating_add(128)
                        })
                        .sum::<usize>(),
                ),
            ) > MAX_INPUT
            {
                navigation_omitted.push(json!({"url":url,"reason":"navigation_input_budget"}));
            } else {
                navigation_bytes += item.url.len().saturating_add(512);
                navigation_pages.push(item.clone());
            }
        }
        let sitemap_origins = page_sitemap(
            &sitemap,
            &seed_url,
            &url,
            facts.in_sitemap,
            &mut sitemap_lookups,
            &mut sitemap_omissions,
        );
        pages.push(PageReadout {
            url,
            facts,
            evidence: json!({
                "state":evidence["state"],
                "reason":evidence["reason"],
                "capture_state":evidence["capture_state"],
                "collectors":evidence["collectors"],
                "page":page_reason,
            }),
            capture,
            indexing,
            seosmith_index,
            canonical,
            alternates: alternate.as_ref().map(|(assessment, _)| assessment.clone()),
            alternate_node: alternate.map(|(_, node)| node),
            sitemap: sitemap_origins,
        });
    }

    // Build canonical graph nodes. Missing/incomplete evidence never becomes a
    // terminal pass; blocked and HTTP-error pages remain explicit negative nodes.
    let mut canonical_nodes = BTreeMap::new();
    for page in &pages {
        let parsed = Url::parse(&page.url).ok();
        let node = match (&page.capture, parsed) {
            (Some(capture), Some(base)) => {
                match integrity_evidence::node_from_capture(capture, &base, &seed_url) {
                    Some(mut node) => {
                        integrity_evidence::apply_target_observation(
                            &mut node,
                            page.facts.status,
                            &page.seosmith_index,
                        );
                        node
                    }
                    None => manual_node(&page.url, &page.facts),
                }
            }
            _ => manual_node(&page.url, &page.facts),
        };
        canonical_nodes.insert(page.url.clone(), node);
    }
    let mut canonical_paths = BTreeMap::new();
    for page in &pages {
        if Url::parse(&page.url).is_ok() {
            canonical_paths.insert(
                page.url.clone(),
                integrity_evidence::canonical_path(
                    &page.url,
                    &canonical_nodes,
                    &seed_url,
                    MAX_CANONICAL_DEPTH,
                ),
            );
        }
    }

    // Build observed alternate clusters without claiming crawl discovery.
    let mut alternate_nodes = BTreeMap::new();
    for page in &pages {
        if let Some(node) = &page.alternate_node {
            alternate_nodes.insert(page.url.clone(), node.clone());
        }
    }
    let mut clusters = Vec::new();
    let mut visited = BTreeSet::new();
    for page in &pages {
        if clusters.len() >= MAX_ALTERNATE_CLUSTERS {
            break;
        }
        let Some(node) = &page.alternate_node else {
            continue;
        };
        if visited.contains(&page.url)
            || !node
                .alternates
                .iter()
                .any(|declaration| declaration.state == DeclarationState::Valid)
        {
            continue;
        }
        let cluster = integrity_evidence::alternate_cluster(
            &page.url,
            &alternate_nodes,
            &seed_url,
            MAX_ALTERNATE_DEPTH,
            false,
        );
        visited.extend(cluster.members.iter().cloned());
        clusters.push(cluster);
    }

    let navigation_coverage = GraphCoverageInput {
        discovery_complete: None,
        expected_pages: if row_limit { None } else { Some(row_count) },
        observed_pages: navigation_pages.len(),
        unavailable_pages: navigation_omitted.len(),
        truncated_pages: navigation_pages
            .iter()
            .filter(|page| !page.extraction_complete)
            .count(),
        omitted_records: if row_limit {
            None
        } else {
            Some(navigation_omitted.len())
        },
        comparisons_complete: Some(true),
    };
    let navigation_nodes = graph_evidence::graph(&seed, &navigation_pages, &navigation_coverage)
        .map_err(|_| anyhow::anyhow!("duplicate page URLs"))?;

    let (findings, findings_omitted) = build_findings(&pages, &canonical_paths, &clusters);

    let mut reasons = BTreeSet::from([
        "terminal_local_state_does_not_prove_discovery_completion",
        "per_page_observation_times_unavailable",
    ]);
    if row_limit {
        reasons.insert("row_limit_reached");
    }
    if !omitted.is_empty() || !navigation_omitted.is_empty() {
        reasons.insert("retained_records_omitted");
    }
    if pages.iter().any(|page| page.capture.is_none()) {
        reasons.insert("page_evidence_incomplete_or_legacy");
    }
    if pages.iter().any(|page| page.facts.blocked) {
        reasons.insert("blocked_pages_present");
    }
    if pages.iter().any(|page| {
        page.indexing
            .iter()
            .any(|index| index.directives.index == Decision::Unknown)
    }) {
        reasons.insert("effective_indexing_unknown_for_some_bots");
    }
    if pages.iter().any(|page| {
        page.canonical
            .as_ref()
            .is_some_and(|canonical| canonical.agreement == CanonicalAgreement::Unknown)
    }) {
        reasons.insert("canonical_evidence_incomplete");
    }
    if pages.iter().any(|page| {
        page.alternates
            .as_ref()
            .is_some_and(|alternates| !alternates.complete)
    }) {
        reasons.insert("alternate_evidence_incomplete");
    }
    if sitemap
        .run
        .as_ref()
        .is_some_and(|run| run.state != "complete" || run.discovery_state != "complete")
    {
        reasons.insert("sitemap_coverage_partial_or_unknown");
    }
    if sitemap.run.is_none() {
        reasons.insert("sitemap_evidence_unavailable_or_legacy");
    }
    if robots["state"] != "observed" {
        reasons.insert("robots_run_unavailable_or_legacy");
    }
    if findings_omitted > 0 || sitemap_omissions > 0 {
        reasons.insert("findings_or_origin_output_bounded");
    }

    let coverage = json!({
        "complete": false,
        "state": "partial",
        "reasons": reasons,
        "rows_seen": row_count,
        "pages_retained": pages.len(),
        "row_limit_reached": row_limit,
        "omitted_records": omitted.len().saturating_add(navigation_omitted.len()).saturating_add(sitemap_omissions),
        "findings_omitted": findings_omitted,
        "limits": {
            "rows": MAX_ROWS,
            "evidence_bytes_per_row": MAX_EVIDENCE,
            "page_json_bytes_per_row": MAX_PAGE_JSON,
            "aggregate_parsed_evidence_bytes": MAX_INPUT,
            "output_bytes": MAX_OUTPUT,
            "omission_records": MAX_OMISSIONS,
            "findings": MAX_FINDINGS,
            "sitemap_origin_lookups": MAX_SITEMAP_LOOKUPS,
        },
    });

    let page_output = pages
        .iter()
        .map(|page| {
            json!({
                "url": page.url,
                "frontier": {
                    "depth": page.facts.depth,
                    "in_sitemap": page.facts.in_sitemap,
                    "resource": page.facts.resource,
                },
                "page": {
                    "status": page.facts.status,
                    "blocked_by_robots": page.facts.blocked,
                    "error": page.facts.error,
                    "content_type": page.facts.content_type,
                },
                "evidence": page.evidence,
                "effective_indexing": page.indexing.iter().map(effective_json).collect::<Vec<_>>(),
                "canonical": page.canonical.as_ref().map(canonical_json),
                "canonical_path": canonical_paths.get(&page.url).map(path_json),
                "alternates": page.alternate_node.as_ref().zip(page.alternates.as_ref()).map(|(node, assessment)| alternates_json(node, assessment)),
                "sitemap_origins": page.sitemap,
            })
        })
        .collect::<Vec<_>>();
    let canonical_graph = pages
        .iter()
        .filter_map(|page| {
            canonical_paths.get(&page.url).map(|path| {
                json!({
                    "source": page.url,
                    "path": path.path,
                    "state": graph_state_name(&path.state),
                })
            })
        })
        .collect::<Vec<_>>();
    let alternate_graph = clusters
        .iter()
        .map(|cluster| {
            json!({
                "members": cluster.members,
                "complete": cluster.complete,
                "discovery_complete": false,
                "edges": cluster.edges.iter().map(|edge| json!({
                    "path": edge.path,
                    "source": edge.source,
                    "language": edge.language,
                    "target": edge.target,
                    "reciprocal": match edge.reciprocal {
                        integrity_evidence::Reciprocity::Present => "present",
                        integrity_evidence::Reciprocity::Missing => "missing",
                        integrity_evidence::Reciprocity::Unknown => "unknown",
                        integrity_evidence::Reciprocity::OutsideScope => "outside_scope",
                    },
                    "declaration": declaration_json(&edge.declaration),
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    let output = serde_json::to_vec(&json!({
        "version": 1,
        "command": "saved-index-integrity",
        "request_id": id,
        "terminal_state": state,
        "completion": completion,
        "seed": seed,
        "scope": {
            "scheme": seed_url.scheme(),
            "host": seed_url.host_str(),
            "port": seed_url.port(),
        },
        "freshness": {
            "state": if started_at.is_some() {"run_interval_only"} else {"unknown"},
            "run_created_unix": created,
            "run_started_at": started_at,
            "page_observed_at": null,
            "sitemap_lastmod": "syntax_only_not_a_freshness_measurement",
            "reason": "terminal checkpoints do not retain per-page observation or completion times",
        },
        "legacy": {
            "report_present": report_present,
            "report_hash": report_hash,
            "delivery_state": delivery_state,
            "note": "report bytes are not read or modified by this command",
        },
        "discovery_coverage": "unknown",
        "coverage": coverage,
        "bots": BOTS,
        "pages": page_output,
        "graphs": {
            "canonical": canonical_graph,
            "alternates": alternate_graph,
            "navigation": {
                "discovery_complete": false,
                "coverage": graph_evidence::graph_coverage(&navigation_coverage),
                "nodes": navigation_nodes,
                "omitted": navigation_omitted,
            },
            "sitemap": sitemap_summary(&sitemap),
        },
        "robots": robots,
        "findings": findings,
        "omitted": omitted,
    }))?;
    ensure!(
        output.len() <= MAX_OUTPUT,
        "analysis output exceeds 4 MiB; no partial output produced"
    );
    println!("{}", std::str::from_utf8(&output)?);
    Ok(())
}
