//! Local robots match provenance, checked against the crawler's matcher.
use robotstxt::{
    RobotsParseHandler,
    matcher::{LongestMatchRobotsMatchStrategy, RobotsMatchStrategy},
};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RobotsDecision {
    pub bot: String,
    pub allowed: bool,
    pub matched_line: Option<u32>,
    pub rule: Option<String>,
    pub raw_line: Option<String>,
    pub provenance: String,
}
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // Mirrors independent parser group flags.
struct Trace {
    bot: String,
    path: String,
    global: bool,
    specific: bool,
    ever_specific: bool,
    separator: bool,
    ga: Option<(i32, u32, String)>,
    gd: Option<(i32, u32, String)>,
    sa: Option<(i32, u32, String)>,
    sd: Option<(i32, u32, String)>,
}
impl Trace {
    fn rule(&mut self, line: u32, value: &str, allow: bool) {
        if !self.global && !self.specific {
            return;
        }
        self.separator = true;
        let strategy = LongestMatchRobotsMatchStrategy;
        let mut priority = strategy.match_disallow(&self.path, value);
        if allow
            && priority < 0
            && let Some(pos) = value.rfind('/')
            && value[pos..].starts_with("/index.htm")
        {
            priority = strategy.match_disallow(&self.path, &format!("{}$", &value[..=pos]));
        }
        if priority <= 0 {
            return;
        }
        let slot = match (self.specific, allow) {
            (true, true) => &mut self.sa,
            (true, false) => &mut self.sd,
            (false, true) => &mut self.ga,
            (false, false) => &mut self.gd,
        };
        if slot.as_ref().is_none_or(|v| v.0 < priority) {
            *slot = Some((priority, line, value.to_owned()));
        }
    }
}
impl RobotsParseHandler for Trace {
    fn handle_robots_start(&mut self) {}
    fn handle_robots_end(&mut self) {}
    fn handle_user_agent(&mut self, _: u32, agent: &str) {
        if self.separator {
            self.global = false;
            self.specific = false;
            self.separator = false;
        }
        if agent.starts_with('*')
            && (agent.len() == 1 || agent[1..].starts_with(char::is_whitespace))
        {
            self.global = true;
        } else {
            let token = agent
                .split(|c: char| !(c.is_ascii_alphabetic() || c == '-' || c == '_'))
                .next()
                .unwrap_or("");
            if token.eq_ignore_ascii_case(&self.bot) {
                self.specific = true;
                self.ever_specific = true;
            }
        }
    }
    fn handle_allow(&mut self, line: u32, value: &str) {
        self.rule(line, value, true);
    }
    fn handle_disallow(&mut self, line: u32, value: &str) {
        self.rule(line, value, false);
    }
    fn handle_sitemap(&mut self, _: u32, _: &str) {
        self.separator = true;
    }
    fn handle_unknown_action(&mut self, _: u32, _: &str, _: &str) {
        self.separator = true;
    }
}
/// Provenance is unavailable if independently reconstructed outcome differs from the crawler.
pub fn evaluate(body: &str, bot: &str, url: &Url) -> RobotsDecision {
    let allowed =
        robotstxt::DefaultMatcher::default().one_agent_allowed_by_robots(body, bot, url.as_str());
    let mut trace = Trace {
        bot: bot.into(),
        path: robotstxt::get_path_params_query(url.as_str()).into_owned(),
        ..Trace::default()
    };
    robotstxt::parse_robotstxt(body, &mut trace);
    let (a, d) = if trace.ever_specific {
        (trace.sa, trace.sd)
    } else {
        (trace.ga, trace.gd)
    };
    let a_priority = a.as_ref().map_or(0, |v| v.0);
    let d_priority = d.as_ref().map_or(0, |v| v.0);
    let reconstructed = d_priority <= a_priority;
    let winner = if d_priority > a_priority { d } else { a };
    let (line, rule, provenance) = if reconstructed != allowed {
        (None, None, "matcher_disagreement")
    } else if let Some((_, line, value)) = winner {
        (Some(line), Some(value), "matched_rule")
    } else {
        (None, None, "no_matching_rule")
    };
    // The parser treats CRLF as one boundary, and bare CR and LF as
    // boundaries too. Use the same line numbering for source attribution.
    let normalized_lines = body.replace("\r\n", "\n").replace('\r', "\n");
    let raw_line = line
        .and_then(|n| usize::try_from(n).ok())
        .and_then(|n| n.checked_sub(1))
        .and_then(|n| normalized_lines.split('\n').nth(n))
        .filter(|s| s.len() <= 16_384)
        .map(str::to_owned);
    let provenance = if line.is_some() && raw_line.is_none() {
        "matched_line_unavailable"
    } else {
        provenance
    };
    RobotsDecision {
        bot: bot.into(),
        allowed,
        matched_line: line,
        rule,
        raw_line,
        provenance: provenance.into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn specific_group_and_longest_match_explain_decision() {
        let url = Url::parse("https://example.com/private/open").unwrap();
        let body = "User-agent: *\nDisallow: /private\nUser-agent: Googlebot\nDisallow: /private\nAllow: /private/open\n";
        let d = evaluate(body, "Googlebot", &url);
        assert!(d.allowed);
        assert_eq!(d.matched_line, Some(5));
        assert_eq!(d.rule.as_deref(), Some("/private/open"));
        assert_eq!(d.raw_line.as_deref(), Some("Allow: /private/open"));
        let d = evaluate(body, "GPTBot", &url);
        assert!(!d.allowed);
        assert_eq!(d.matched_line, Some(2));
    }
    #[test]
    fn rules_cover_ties_wildcards_and_unmatched() {
        let url = Url::parse("https://example.com/private/doc").unwrap();
        let cases = [
            (
                "User-agent: *\nDisallow: /private\nAllow: /private\n",
                true,
                Some(3),
            ),
            ("User-agent: *\nDisallow: /private/*\n", false, Some(2)),
            ("User-agent: OtherBot\nDisallow: /\n", true, None),
            (
                "User-agent: *\nDisallow: /private\nUser-agent: GPTBot\nDisallow:\n",
                true,
                None,
            ),
        ];
        for (body, allowed, line) in cases {
            let d = evaluate(body, "GPTBot", &url);
            assert_eq!(d.allowed, allowed, "{body}");
            assert_eq!(d.matched_line, line, "{body}");
            assert_ne!(d.provenance, "matcher_disagreement");
        }
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    #[test]
    fn paired_named_bot_fixture() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/s02-s03/robots-cases.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let bot = case["bot"].as_str().unwrap();
            let body = case["body"].as_str().unwrap();
            let url = Url::parse(case["url"].as_str().unwrap()).unwrap();
            let decision = evaluate(body, bot, &url);
            assert_eq!(
                decision.allowed,
                case["allowed"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                decision.matched_line,
                case["line"].as_u64().map(|v| u32::try_from(v).unwrap()),
                "{}",
                case["name"]
            );
            assert_ne!(decision.provenance, "matcher_disagreement");
        }
    }
}
