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
#[derive(Clone, Debug, PartialEq, Eq)]
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
                .map(|v| v.to_ascii_lowercase()),
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
        content_state: state.clone(),
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
