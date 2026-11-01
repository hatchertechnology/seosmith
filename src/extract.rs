//! Bounded, source-only extraction. No script execution or remote JSON-LD contexts.
use crate::model::{Page, Relationship, StructuredData, normalize};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

pub const LIMIT: usize = 2000;
fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}
fn text(el: ElementRef<'_>) -> String {
    el.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(4096)
        .collect()
}
pub fn relationship(
    p: &mut Page,
    base: &Url,
    kind: &str,
    raw: &str,
    descriptor: &str,
    resource: bool,
) {
    if raw.trim().is_empty() {
        return;
    }
    if let Some(url) = normalize(base, raw) {
        if p.relationships.len() >= LIMIT {
            p.extraction_truncated = true;
            return;
        }
        let url = url.to_string();
        if !p
            .relationships
            .iter()
            .any(|r| r.kind == kind && r.url == url && r.descriptor == descriptor)
        {
            p.relationships.push(Relationship {
                kind: kind.into(),
                url,
                descriptor: descriptor.chars().take(256).collect(),
                resource,
                state: "not_fetched".into(),
            });
        }
    }
}
/// Parse srcset candidates without splitting commas inside URLs (e.g. data URLs).
pub fn srcset(value: &str) -> Vec<(String, String)> {
    let mut rest = value;
    let mut out = Vec::new();
    while !rest.is_empty() && out.len() < LIMIT + 1 {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..end];
        rest = &rest[end..];
        if token.is_empty() {
            break;
        }
        if token.ends_with(',') {
            out.push((token.trim_end_matches(',').into(), String::new()));
            continue;
        }
        let end = rest.find(',').unwrap_or(rest.len());
        let descriptor = rest[..end].trim().to_owned();
        rest = rest.get(end + 1..).unwrap_or_default();
        out.push((token.into(), descriptor));
    }
    out
}
/// Tokenize CSS including nested rules, quoted `url()`, and @import strings.
pub fn css_urls(css: &str) -> Vec<String> {
    css_observations(css).0
}
pub fn css_observations(css: &str) -> (Vec<String>, bool) {
    fn walk(
        parser: &mut cssparser::Parser<'_>,
        out: &mut Vec<String>,
        depth: usize,
        tokens: &mut usize,
        truncated: &mut bool,
    ) {
        let mut import = false;
        while *tokens < 20_000 && out.len() <= LIMIT {
            let Ok(token) = parser.next().cloned() else {
                break;
            };
            *tokens += 1;
            match token {
                cssparser::Token::UnquotedUrl(u) => out.push(u.to_string()),
                cssparser::Token::AtKeyword(k) => import = k.eq_ignore_ascii_case("import"),
                cssparser::Token::QuotedString(u) if import => {
                    out.push(u.to_string());
                    import = false;
                }
                cssparser::Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                    let value: Result<String, cssparser::ParseError<()>> =
                        parser.parse_nested_block(|p| Ok(p.expect_string_cloned()?.to_string()));
                    if let Ok(u) = value {
                        out.push(u);
                    }
                }
                cssparser::Token::Function(_)
                | cssparser::Token::CurlyBracketBlock
                | cssparser::Token::ParenthesisBlock
                | cssparser::Token::SquareBracketBlock
                    if depth < 16 =>
                {
                    let _: Result<(), cssparser::ParseError<()>> = parser.parse_nested_block(|p| {
                        walk(p, out, depth + 1, tokens, truncated);
                        Ok(())
                    });
                }
                cssparser::Token::Function(_)
                | cssparser::Token::CurlyBracketBlock
                | cssparser::Token::ParenthesisBlock
                | cssparser::Token::SquareBracketBlock => *truncated = true,
                _ => {}
            }
        }
        if *tokens >= 20_000 || out.len() > LIMIT {
            *truncated = true;
        }
    }
    let mut out = Vec::new();
    let mut truncated = false;
    walk(
        &mut cssparser::Parser::new(css),
        &mut out,
        0,
        &mut 0,
        &mut truncated,
    );
    (out, truncated)
}
fn charset(value: &str) -> Option<&'static encoding_rs::Encoding> {
    value.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| {
                encoding_rs::Encoding::for_label(v.trim().trim_matches(['\'', '"']).as_bytes())
            })
            .flatten()
    })
}
pub fn decode(body: &[u8], content_type: &str) -> (String, String, bool) {
    let sample = String::from_utf8_lossy(&body[..body.len().min(1024)]);
    let doc = Html::parse_document(&sample);
    let declared = doc.select(&sel("meta")).find_map(|m| {
        m.value()
            .attr("charset")
            .and_then(|s| encoding_rs::Encoding::for_label(s.as_bytes()))
            .or_else(|| {
                m.value()
                    .attr("http-equiv")
                    .filter(|s| s.eq_ignore_ascii_case("content-type"))
                    .and_then(|_| m.value().attr("content"))
                    .and_then(charset)
            })
    });
    let encoding = encoding_rs::Encoding::for_bom(body)
        .map(|(e, _)| e)
        .or_else(|| charset(content_type))
        .or(declared)
        .unwrap_or(encoding_rs::UTF_8);
    let (value, used, errors) = encoding.decode(body);
    (value.into_owned(), used.name().into(), errors)
}
/// Split only outside quoted strings and angle-bracket URL targets.
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
pub fn headers(p: &mut Page, base: &Url, value: &str) {
    for entry in split_header(value, ',').into_iter().take(LIMIT + 1) {
        let Some((target, attrs)) = entry
            .trim()
            .strip_prefix('<')
            .and_then(|s| s.split_once('>'))
        else {
            continue;
        };
        let mut rel = String::new();
        let mut lang = None;
        for attr in split_header(attrs, ';') {
            if let Some((k, v)) = attr.trim().split_once('=') {
                let v = v.trim().trim_matches('"');
                if k.trim().eq_ignore_ascii_case("rel") {
                    rel = v.to_ascii_lowercase();
                }
                if k.trim().eq_ignore_ascii_case("hreflang") {
                    lang = Some(v.to_owned());
                }
            }
        }
        if rel.split_whitespace().any(|r| r == "canonical")
            && let Some(url) = normalize(base, target)
        {
            p.header_canonical = Some(url.to_string());
            relationship(p, base, "canonical_header", target, "", false);
        }
        if rel.split_whitespace().any(|r| r == "alternate")
            && let (Some(language), Some(url)) = (lang, normalize(base, target))
        {
            relationship(p, base, "hreflang_header", target, &language, false);
            p.alternates.push(crate::model::Alternate {
                language,
                url: url.to_string(),
            });
        }
    }
}
fn micro_value(el: ElementRef<'_>, base: &Url) -> Value {
    let tag = el.value().name();
    let attr = match tag {
        "meta" => Some("content"),
        "audio" | "embed" | "iframe" | "img" | "source" | "track" | "video" => Some("src"),
        "a" | "area" | "link" => Some("href"),
        "object" => Some("data"),
        "data" | "meter" => Some("value"),
        "time" => Some("datetime"),
        _ => None,
    };
    let value = attr
        .and_then(|a| el.value().attr(a))
        .map_or_else(|| text(el), |s| s.chars().take(4096).collect());
    if matches!(attr, Some("src" | "href" | "data")) {
        json!(normalize(base, &value).map_or(value, Into::into))
    } else {
        json!(value)
    }
}
fn micro_item(
    el: ElementRef<'_>,
    doc: &Html,
    base: &Url,
    depth: usize,
    budget: &mut usize,
) -> Value {
    let types: Vec<_> = el
        .value()
        .attr("itemtype")
        .unwrap_or_default()
        .split_whitespace()
        .take(32)
        .map(|s| s.chars().take(2048).collect::<String>())
        .collect();
    let id = el
        .value()
        .attr("itemid")
        .and_then(|s| normalize(base, s))
        .map(|u| u.to_string());
    let mut properties = serde_json::Map::<String, Value>::new();
    let mut pending: Vec<_> = el.children().filter_map(ElementRef::wrap).collect();
    for reference in el
        .value()
        .attr("itemref")
        .unwrap_or_default()
        .split_whitespace()
        .take(32)
    {
        if let Some(node) = doc
            .select(&sel("[id]"))
            .find(|n| n.value().attr("id") == Some(reference))
        {
            pending.push(node);
        }
    }
    pending.reverse();
    let mut visited = vec![el.id()];
    while let Some(node) = pending.pop() {
        if *budget == 0 {
            break;
        }
        if visited.contains(&node.id()) {
            continue;
        }
        visited.push(node.id());
        *budget -= 1;
        let nested = node.value().attr("itemscope").is_some();
        let names: Vec<_> = node
            .value()
            .attr("itemprop")
            .unwrap_or_default()
            .split_whitespace()
            .take(32)
            .collect();
        if !names.is_empty() {
            let value = if nested && depth < 16 {
                micro_item(node, doc, base, depth + 1, budget)
            } else if nested {
                *budget = 0;
                Value::Null
            } else {
                micro_value(node, base)
            };
            for name in names {
                properties
                    .entry(name.chars().take(256).collect::<String>())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .expect("array property")
                    .push(value.clone());
            }
        }
        if !nested {
            pending.extend(node.children().filter_map(ElementRef::wrap).rev());
        }
    }
    json!({"types":types,"id":id,"properties":properties})
}
pub fn enrich(p: &mut Page, doc: &Html, base: &Url) {
    for selector in [
        "meta",
        "script[src],link[href],iframe[src],audio[src],audio source[src],video[src],video source[src],video[poster],img[srcset],picture source[srcset]",
        "style,[style]",
    ] {
        if doc.select(&sel(selector)).take(LIMIT + 1).count() > LIMIT {
            p.extraction_truncated = true;
        }
    }
    for el in doc.select(&sel("meta")).take(LIMIT + 1) {
        let key = el
            .value()
            .attr("property")
            .or_else(|| el.value().attr("name"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        let value = el.value().attr("content").unwrap_or_default();
        if key.starts_with("og:") || key.starts_with("twitter:") {
            let map = if key.starts_with("og:") {
                &mut p.open_graph
            } else {
                &mut p.twitter
            };
            if map.values().map(Vec::len).sum::<usize>() < 128 {
                map.entry(key.chars().take(256).collect())
                    .or_default()
                    .push(value.chars().take(4096).collect());
                if value.chars().count() > 4096 {
                    p.extraction_truncated = true;
                }
            } else {
                p.extraction_truncated = true;
            }
        }
        if el
            .value()
            .attr("http-equiv")
            .is_some_and(|s| s.eq_ignore_ascii_case("refresh"))
        {
            if let Some((delay, target)) = value.split_once(';') {
                p.refresh_delay = delay
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n >= 0.0);
                if let Some((key, raw)) = target.trim().split_once('=')
                    && key.trim().eq_ignore_ascii_case("url")
                    && p.refresh_delay.is_some()
                {
                    p.refresh_target =
                        normalize(base, raw.trim().trim_matches(['\'', '"'])).map(Into::into);
                }
            } else {
                p.refresh_delay = value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n >= 0.0);
            }
        }
    }
    for el in doc.select(&sel("script[type]")).filter(|e| {
        e.value().attr("type").is_some_and(|s| {
            s.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/ld+json")
        })
    }) {
        if p.structured_data.len() >= 64 {
            p.extraction_truncated = true;
            break;
        }
        let raw = el.text().collect::<String>();
        let (data, error) = if raw.len() > 65_536 {
            p.extraction_truncated = true;
            (None, Some("block_limit".into()))
        } else {
            match serde_json::from_str::<Value>(&raw) {
                Ok(v) if v.is_array() || v.is_object() => (Some(v), None),
                Ok(_) => (None, Some("expected_object_or_array".into())),
                Err(e) => (
                    None,
                    Some(format!("invalid_json at {}:{}", e.line(), e.column())),
                ),
            }
        };
        p.structured_data.push(StructuredData {
            format: "json_ld".into(),
            data,
            error,
        });
    }
    let mut budget = LIMIT;
    for el in doc.select(&sel("[itemscope]")).filter(|e| {
        !e.ancestors()
            .filter_map(ElementRef::wrap)
            .any(|p| p.value().attr("itemscope").is_some())
    }) {
        if p.structured_data.len() >= 64 || budget == 0 {
            p.extraction_truncated = true;
            break;
        }
        let data = micro_item(el, doc, base, 0, &mut budget);
        p.structured_data.push(StructuredData {
            format: "microdata".into(),
            data: Some(data),
            error: None,
        });
    }
    if budget == 0 {
        p.extraction_truncated = true;
    }
    // Bound retained metadata independently from the response body.
    while serde_json::to_vec(&(&p.structured_data, &p.open_graph, &p.twitter))
        .is_ok_and(|v| v.len() > 262_144)
    {
        p.extraction_truncated = true;
        if p.structured_data.pop().is_none() && p.open_graph.pop_last().is_none() {
            p.twitter.pop_last();
        }
    }
    let dependencies = sel(concat!(
        "script[src],link[href],iframe[src],audio[src],audio source[src],",
        "video[src],video source[src],video[poster],img[srcset],picture source[srcset]"
    ));
    for el in doc.select(&dependencies).take(LIMIT + 1) {
        let tag = el.value().name();
        if let Some(set) = el.value().attr("srcset") {
            for (url, descriptor) in srcset(set) {
                relationship(p, base, "image_candidate", &url, &descriptor, true);
            }
        }
        if let Some(poster) = el.value().attr("poster") {
            relationship(p, base, "video_poster", poster, "", true);
        }
        if tag == "link" {
            let rel = el
                .value()
                .attr("rel")
                .unwrap_or_default()
                .to_ascii_lowercase();
            for kind in ["stylesheet", "preload", "modulepreload"] {
                if rel.split_whitespace().any(|r| r == kind) {
                    relationship(
                        p,
                        base,
                        kind,
                        el.value().attr("href").unwrap_or_default(),
                        "",
                        true,
                    );
                }
            }
        } else if let Some(src) = el.value().attr("src") {
            relationship(p, base, tag, src, "", true);
        }
    }
    for el in doc.select(&sel("style,[style]")).take(LIMIT + 1) {
        let css = if el.value().name() == "style" {
            el.text().collect::<String>()
        } else {
            el.value().attr("style").unwrap_or_default().into()
        };
        let (urls, truncated) = css_observations(&css);
        p.extraction_truncated |= truncated;
        for url in urls {
            relationship(p, base, "css_url", &url, "", true);
        }
    }
}
