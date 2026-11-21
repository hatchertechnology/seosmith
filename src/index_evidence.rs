//! Local-only indexing evidence. This module is not part of any delivered report.
use std::collections::BTreeSet;

/// A directive exactly as collected, before bot applicability is evaluated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectiveSource {
    pub source: Source,
    /// `None` means generic; a named source applies only to that bot.
    pub bot: Option<String>,
    pub value: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    HtmlMeta,
    HttpHeader,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentState {
    Available,
    Blocked,
    FetchFailed,
    Truncated,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Restricted,
    Allowed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEvaluation {
    pub bot: String,
    pub content_state: ContentState,
    pub sources: Vec<DirectiveSource>,
    pub applicable: Vec<DirectiveSource>,
    pub restrictions: BTreeSet<String>,
    pub conflicts: Vec<String>,
    pub index: Decision,
    pub follow: Decision,
}

/// Evaluates generic and exactly named directives independently for each bot.
/// Missing content is never treated as permission. `all` cannot override a restriction.
pub fn evaluate(bot: &str, state: ContentState, sources: &[DirectiveSource]) -> IndexEvaluation {
    let applicable: Vec<_> = sources
        .iter()
        .filter(|s| s.bot.as_deref().is_none_or(|b| b.eq_ignore_ascii_case(bot)))
        .cloned()
        .collect();
    let mut tokens = BTreeSet::new();
    for s in &applicable {
        tokens.extend(
            s.value
                .split([',', ';'])
                .flat_map(str::split_whitespace)
                .map(str::to_ascii_lowercase),
        );
    }
    let mut restrictions = BTreeSet::new();
    for token in &tokens {
        if matches!(
            token.as_str(),
            "noindex"
                | "nofollow"
                | "none"
                | "noarchive"
                | "nosnippet"
                | "noimageindex"
                | "notranslate"
        ) || token.starts_with("max-snippet:")
            || token.starts_with("max-image-preview:")
            || token.starts_with("max-video-preview:")
            || token.starts_with("unavailable_after:")
        {
            restrictions.insert(token.clone());
        }
    }
    let noindex = tokens.contains("noindex") || tokens.contains("none");
    let nofollow = tokens.contains("nofollow") || tokens.contains("none");
    let mut conflicts = Vec::new();
    if noindex && (tokens.contains("index") || tokens.contains("all")) {
        conflicts.push("index/noindex".into());
    }
    if nofollow && (tokens.contains("follow") || tokens.contains("all")) {
        conflicts.push("follow/nofollow".into());
    }
    let decide = |restricted| {
        if restricted {
            Decision::Restricted
        } else if state == ContentState::Available {
            Decision::Allowed
        } else {
            Decision::Unknown
        }
    };
    IndexEvaluation {
        bot: bot.into(),
        content_state: state,
        sources: sources.to_vec(),
        applicable,
        restrictions,
        conflicts,
        index: decide(noindex),
        follow: decide(nofollow),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn applicable_restrictions_win_and_bots_are_independent() {
        let sources = vec![
            DirectiveSource {
                source: Source::HtmlMeta,
                bot: None,
                value: "index, follow".into(),
            },
            DirectiveSource {
                source: Source::HttpHeader,
                bot: Some("Googlebot".into()),
                value: "noindex, nofollow".into(),
            },
            DirectiveSource {
                source: Source::HtmlMeta,
                bot: Some("GPTBot".into()),
                value: "noindex".into(),
            },
        ];
        let google = evaluate("googlebot", ContentState::Available, &sources);
        assert_eq!(google.index, Decision::Restricted);
        assert_eq!(google.follow, Decision::Restricted);
        assert_eq!(google.conflicts.len(), 2);
        assert_eq!(
            evaluate("GPTBot", ContentState::Available, &sources).follow,
            Decision::Allowed
        );
        assert_eq!(
            evaluate("OAI-SearchBot", ContentState::Available, &sources).index,
            Decision::Allowed
        );
    }
    #[test]
    fn unavailable_content_never_passes() {
        assert_eq!(
            evaluate("Googlebot", ContentState::Blocked, &[]).index,
            Decision::Unknown
        );
        assert_eq!(
            evaluate("Googlebot", ContentState::Truncated, &[]).follow,
            Decision::Unknown
        );
    }
}

/// Desired indexing is supplied by an explicit, independently scoped policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    Index,
    Exclude,
    Unspecified,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentResult {
    Conforms,
    Conflicts,
    Unknown,
}
pub fn compare_intent(index: &Decision, intent: &Intent) -> IntentResult {
    match (index, intent) {
        (Decision::Restricted, Intent::Exclude) | (Decision::Allowed, Intent::Index) => {
            IntentResult::Conforms
        }
        (Decision::Restricted, Intent::Index) | (Decision::Allowed, Intent::Exclude) => {
            IntentResult::Conflicts
        }
        _ => IntentResult::Unknown,
    }
}
#[cfg(test)]
mod intent_tests {
    use super::*;
    #[test]
    fn exclusions_are_not_implicitly_defects() {
        assert_eq!(
            compare_intent(&Decision::Restricted, &Intent::Exclude),
            IntentResult::Conforms
        );
        assert_eq!(
            compare_intent(&Decision::Restricted, &Intent::Unspecified),
            IntentResult::Unknown
        );
        assert_eq!(
            compare_intent(&Decision::Unknown, &Intent::Index),
            IntentResult::Unknown
        );
        assert_eq!(
            compare_intent(&Decision::Restricted, &Intent::Index),
            IntentResult::Conflicts
        );
    }
}

/// Interpret an explicitly requested bot's declarations. Unknown header syntax makes
/// negative conclusions unavailable; retained raw occurrences remain authoritative.
pub fn evaluate_capture(
    bot: &str,
    capture: &crate::collection_evidence::PageCapture,
) -> IndexEvaluation {
    let mut sources = Vec::new();
    let mut complete = capture.state == "available";
    if let Some(http) = &capture.http {
        complete &= http.complete;
        for field in http
            .occurrences
            .iter()
            .filter(|o| o.source == "x-robots-tag")
        {
            let Some(raw) = &field.raw_value else {
                complete = false;
                continue;
            };
            let mut agent: Option<String> = None;
            for part in raw.split(',') {
                let mut value = part.trim();
                if let Some((prefix, suffix)) = value.split_once(':') {
                    let key = prefix.trim().to_ascii_lowercase();
                    if matches!(
                        key.as_str(),
                        "max-snippet" | "max-image-preview" | "max-video-preview"
                    ) {
                        // Parameter is part of a directive, not an agent prefix.
                    } else if key == "unavailable_after" {
                        // Dates can contain commas; do not infer absence from an ambiguous parse.
                        complete = false;
                    } else if !key.is_empty()
                        && key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        agent = Some(key);
                        value = suffix.trim();
                    } else {
                        complete = false;
                    }
                }
                if !value.is_empty() {
                    sources.push(DirectiveSource {
                        source: Source::HttpHeader,
                        bot: agent.clone(),
                        value: value.into(),
                    });
                }
            }
        }
    } else {
        complete = false;
    }
    if let Some(html) = &capture.html {
        complete &= html.complete;
        for meta in html
            .occurrences
            .iter()
            .filter(|o| o.source == "html_head_meta")
        {
            let Some(name) = meta.raw_name.as_deref() else {
                continue;
            };
            let name = name.trim();
            if name.eq_ignore_ascii_case("robots") || name.eq_ignore_ascii_case(bot) {
                if let Some(value) = &meta.raw_value {
                    sources.push(DirectiveSource {
                        source: Source::HtmlMeta,
                        bot: (!name.eq_ignore_ascii_case("robots")).then(|| name.to_owned()),
                        value: value.clone(),
                    });
                } else {
                    complete = false;
                }
            }
        }
    } else {
        complete = false;
    }
    let state = if capture.state == "robots_blocked" {
        ContentState::Blocked
    } else if capture.state == "fetch_failed" {
        ContentState::FetchFailed
    } else if complete {
        ContentState::Available
    } else {
        ContentState::Truncated
    };
    evaluate(bot, state, &sources)
}
#[cfg(test)]
mod capture_tests {
    use super::*;
    #[test]
    fn header_and_html_bots_are_distinct_and_partial_is_unknown() {
        let mut capture = crate::collection_evidence::PageCapture::unavailable(
            "https://example.com/",
            "available",
        );
        capture.http = Some(crate::collection_evidence::http_occurrences(&[
            ("x-robots-tag", "Googlebot: noindex, nofollow"),
            ("x-robots-tag", "GPTBot: noindex"),
        ]));
        capture.html = Some(crate::collection_evidence::html_occurrences(
            "<head><meta name='robots' content='index'><meta name='OAI-SearchBot' content='noindex'></head>",
            true,
        ));
        assert_eq!(
            evaluate_capture("Googlebot", &capture).index,
            Decision::Restricted
        );
        assert_eq!(
            evaluate_capture("GPTBot", &capture).follow,
            Decision::Allowed
        );
        assert_eq!(
            evaluate_capture("OAI-SearchBot", &capture).index,
            Decision::Restricted
        );
        assert_eq!(
            evaluate_capture("Bingbot", &capture).index,
            Decision::Allowed
        );
        capture.html.as_mut().unwrap().complete = false;
        assert_eq!(
            evaluate_capture("Bingbot", &capture).index,
            Decision::Unknown
        );
    }
}

#[cfg(test)]
mod unavailable_tests {
    use super::*;
    #[test]
    fn blocked_noindex_cannot_be_inferred() {
        let capture = crate::collection_evidence::PageCapture::unavailable(
            "https://example.com/blocked",
            "robots_blocked",
        );
        let result = evaluate_capture("Googlebot", &capture);
        assert_eq!(result.index, Decision::Unknown);
        assert_eq!(result.sources.len(), 0);
    }
}
