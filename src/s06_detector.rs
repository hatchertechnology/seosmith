//! Local S06 prototype. Deliberately not registered with the scanner or wire contract.
use scraper::{ElementRef, Html, Selector};
use std::collections::BTreeSet;

const MAX_BYTES: usize = 256 * 1024;
const MAX_TEXT: usize = 32 * 1024;
pub const SEMANTICS: &str = "s06-local-detector-1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Candidate,
    NoCandidate,
    Unknown,
}
#[derive(Debug)]
pub struct Source<'a> {
    pub bytes: Option<&'a [u8]>,
    pub status: Option<u16>,
    pub content_type: &'a str,
    pub complete: bool,
}
#[derive(Debug)]
pub struct Evidence {
    pub source_sha256: String,
    pub main_sha256: String,
    pub title: String,
    pub h1: String,
    pub excerpt: String,
    pub template_fraction: f64,
    main: String,
}
#[derive(Debug)]
pub struct Analysis {
    pub decision: Decision,
    pub reason: &'static str,
    pub evidence: Option<Evidence>,
    pub similarity: Option<f64>,
}
fn result(
    decision: Decision,
    reason: &'static str,
    evidence: Option<Evidence>,
    similarity: Option<f64>,
) -> Analysis {
    Analysis {
        decision,
        reason,
        evidence,
        similarity,
    }
}
fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}
fn text(el: ElementRef<'_>) -> String {
    el.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn extract(source: &Source<'_>) -> Result<Evidence, &'static str> {
    if !source.complete {
        return Err("source unavailable or incomplete");
    }
    let bytes = source.bytes.ok_or("source unavailable")?;
    if bytes.len() > MAX_BYTES {
        return Err("source exceeds byte limit");
    }
    if !source
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("text/html")
    {
        return Err("not HTML");
    }
    let (decoded, _, errors) = seosmith::extract::decode(bytes, source.content_type);
    if errors {
        return Err("decoding errors");
    }
    let doc = Html::parse_document(&decoded);
    let title = doc
        .select(&selector("head title"))
        .next()
        .map_or_else(String::new, text);
    let h1 = doc
        .select(&selector("h1"))
        .next()
        .map_or_else(String::new, text);
    let main = doc
        .select(&selector("main, article, [role=main]"))
        .next()
        .map_or_else(String::new, text);
    let body = doc
        .select(&selector("body"))
        .next()
        .map_or_else(String::new, text);
    if main.len() > MAX_TEXT
        || body.len() > MAX_TEXT
        || title.len() > MAX_TEXT
        || h1.len() > MAX_TEXT
    {
        return Err("text exceeds limit");
    }
    let fraction = if body.is_empty() {
        0.0
    } else {
        1.0 - (f64::from(u32::try_from(main.len().min(body.len())).expect("bounded text"))
            / f64::from(u32::try_from(body.len()).expect("bounded text")))
    };
    let excerpt = main.chars().take(240).collect();
    Ok(Evidence {
        source_sha256: seosmith::model::digest(bytes),
        main_sha256: seosmith::model::digest(main.to_lowercase().as_bytes()),
        title,
        h1,
        excerpt,
        template_fraction: fraction,
        main,
    })
}
fn tokens(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|x| !x.is_empty())
        .map(str::to_lowercase)
        .collect()
}
fn similarity(a: &str, b: &str) -> f64 {
    let a = tokens(a);
    let b = tokens(b);
    let union = a.union(&b).count();
    if union == 0 {
        0.0
    } else {
        f64::from(u32::try_from(a.intersection(&b).count()).expect("bounded text"))
            / f64::from(u32::try_from(union).expect("bounded text"))
    }
}
/// Assess a single retained response. Unknown is distinct from no candidate.
pub fn soft_404(source: &Source<'_>) -> Analysis {
    let evidence = match extract(source) {
        Ok(v) => v,
        Err(e) => return result(Decision::Unknown, e, None, None),
    };
    match source.status {
        Some(404 | 410) => {
            return result(
                Decision::NoCandidate,
                "explicit missing status",
                Some(evidence),
                None,
            );
        }
        Some(200) => (),
        _ => {
            return result(
                Decision::Unknown,
                "status unavailable or ineligible",
                Some(evidence),
                None,
            );
        }
    }
    if evidence.main.trim().is_empty() || evidence.main.trim().eq_ignore_ascii_case("loading…") {
        return result(
            Decision::Unknown,
            "main content or rendered outcome unavailable",
            Some(evidence),
            None,
        );
    }
    let title = evidence.title.to_lowercase();
    let heading = evidence.h1.to_lowercase();
    let error_title = ["page not found", "404 not found"]
        .iter()
        .any(|p| title.trim() == *p);
    let error_heading = [
        "page not found",
        "we could not find that page",
        "404 not found",
    ]
    .iter()
    .any(|p| heading.trim() == *p);
    if error_title && error_heading {
        result(
            Decision::Candidate,
            "matching error title and heading with HTTP 200",
            Some(evidence),
            None,
        )
    } else {
        result(
            Decision::NoCandidate,
            "no matching error pattern",
            Some(evidence),
            None,
        )
    }
}
/// Compare two explicitly supplied retained responses; no peer discovery is performed.
pub fn compare(source: &Source<'_>, peer: Option<&Source<'_>>) -> Analysis {
    let a = match extract(source) {
        Ok(v) => v,
        Err(e) => return result(Decision::Unknown, e, None, None),
    };
    let Some(peer) = peer else {
        return result(
            Decision::Unknown,
            "comparison peer unavailable",
            Some(a),
            None,
        );
    };
    let b = match extract(peer) {
        Ok(v) => v,
        Err(e) => return result(Decision::Unknown, e, Some(a), None),
    };
    if source.status != Some(200)
        || peer.status != Some(200)
        || a.main.is_empty()
        || b.main.is_empty()
    {
        return result(
            Decision::Unknown,
            "comparison requires complete 200 main content",
            Some(a),
            None,
        );
    }
    let score = similarity(&a.main, &b.main);
    if a.source_sha256 == b.source_sha256 {
        return result(
            Decision::Candidate,
            "identical source bytes",
            Some(a),
            Some(score),
        );
    }
    // Explicit SKU markers indicate a plausible product variant, not proof of value.
    if tokens(&a.main).contains("sku") && tokens(&b.main).contains("sku") {
        return result(
            Decision::NoCandidate,
            "explicit SKU variant",
            Some(a),
            Some(score),
        );
    }
    if score >= 0.8 {
        result(
            Decision::Candidate,
            "similar main text",
            Some(a),
            Some(score),
        )
    } else {
        result(
            Decision::NoCandidate,
            "main text below similarity threshold",
            Some(a),
            Some(score),
        )
    }
}
