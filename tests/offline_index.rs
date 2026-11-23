use seosmith::{
    collection_evidence::{PageCapture, html_occurrences, http_occurrences},
    index_evidence::{self, ContentState, CrawlPermission, Decision, Intent, IntentResult, Source},
    robots_evidence,
};
use url::Url;

#[test]
fn retained_sources_named_bots_and_intent_remain_independent() {
    let mut page = PageCapture::unavailable("https://example.test/private", "available");
    page.http = Some(http_occurrences(&[
        ("X-Robots-Tag", "Googlebot: noindex, nofollow"),
        ("X-Robots-Tag", "GPTBot: noindex"),
    ]));
    page.html = Some(html_occurrences(
        "<html><head><meta name='robots' content='index, follow'><meta name='OAI-SearchBot' content='noindex'></head></html>",
        true,
    ));
    page.robots_source = "robots_txt".into();
    let url = Url::parse(&page.url).unwrap();
    let robots = "User-agent: GPTBot\nDisallow: /private\nUser-agent: Googlebot\nAllow: /private\n";
    for bot in ["Googlebot", "GPTBot", "OAI-SearchBot"] {
        page.robots
            .push(robots_evidence::evaluate(robots, bot, &url));
    }
    let google = index_evidence::effective_indexing("Googlebot", &page, &Intent::Index);
    assert_eq!(google.crawl, CrawlPermission::Allowed);
    assert_eq!(google.directives.index, Decision::Restricted);
    assert_eq!(google.intent, IntentResult::Conflicts);
    assert!(
        google
            .directives
            .conflicts
            .contains(&"index/noindex".to_string())
    );
    assert!(google.directives.applicable.iter().any(|s| {
        s.source == Source::HttpHeader
            && s.locator
                .as_ref()
                .is_some_and(|l| l.ordinal == 0 && l.part == Some(0))
    }));
    assert!(google.directives.applicable.iter().any(
        |s| s.source == Source::HtmlMeta && s.locator.as_ref().is_some_and(|l| l.ordinal == 0)
    ));
    let gpt = index_evidence::effective_indexing("GPTBot", &page, &Intent::Unspecified);
    assert_eq!(gpt.crawl, CrawlPermission::Denied);
    assert_eq!(
        gpt.robots.unwrap().raw_line.as_deref(),
        Some("Disallow: /private")
    );
    assert_eq!(gpt.directives.follow, Decision::Allowed);
    assert_eq!(gpt.intent, IntentResult::Unknown);
    assert_eq!(
        index_evidence::evaluate_capture("OAI-SearchBot", &page).index,
        Decision::Restricted
    );
    assert_eq!(
        index_evidence::evaluate_capture("Bingbot", &page).index,
        Decision::Allowed
    );
}

#[test]
fn blocked_or_omitted_evidence_cannot_establish_permission() {
    let mut page = PageCapture::unavailable("https://example.test/private", "robots_blocked");
    let blocked = index_evidence::effective_indexing("Googlebot", &page, &Intent::Exclude);
    assert_eq!(blocked.directives.content_state, ContentState::Blocked);
    assert_eq!(blocked.directives.index, Decision::Unknown);
    assert_eq!(blocked.intent, IntentResult::Unknown);
    page.state = "available".into();
    page.http = Some(http_occurrences(&[]));
    page.html = Some(html_occurrences("<head></head>", true));
    page.html.as_mut().unwrap().omitted = 1;
    assert_eq!(
        index_evidence::evaluate_capture("Googlebot", &page).index,
        Decision::Unknown
    );
}
