use anyhow::{Context, Result, bail, ensure};
use ipnet::IpNet;
use reqwest::{Client, redirect::Policy};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use url::Url;

pub const MAX_BODY: usize = 4 * 1024 * 1024;
#[derive(Clone)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub local_http_occurrences: Option<crate::collection_evidence::Collection>,
    pub body: Vec<u8>,
    pub ttfb_ms: u64,
}
#[derive(Clone)]
pub struct Fetcher {
    pub user_agent: String,
}
impl Default for Fetcher {
    fn default() -> Self {
        Self {
            user_agent: format!(
                "Seosmith/{} (+https://github.com/hatchertechnology/seosmith)",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }
}
pub fn public_ip(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    };
    let excluded = [
        "0.0.0.0/8",
        "10.0.0.0/8",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.0.0.0/24",
        "192.0.2.0/24",
        "192.168.0.0/16",
        "198.18.0.0/15",
        "198.51.100.0/24",
        "203.0.113.0/24",
        "224.0.0.0/3",
        "192.88.99.0/24",
        "::/96",
        "::ffff:0:0/96",
        "64:ff9b::/96",
        "64:ff9b:1::/48",
        "100::/64",
        "2001::/23",
        "2001:db8::/32",
        "2002::/16",
        "3fff::/20",
        "fc00::/7",
        "fe80::/10",
        "ff00::/8",
    ];
    if excluded
        .iter()
        .any(|s| s.parse::<IpNet>().is_ok_and(|n| n.contains(&ip)))
    {
        return false;
    }
    match ip {
        IpAddr::V4(_) => true,
        IpAddr::V6(v) => "2000::/3"
            .parse::<IpNet>()
            .is_ok_and(|n| n.contains(&IpAddr::V6(v))),
    }
}
pub trait Transport: Sync {
    fn get(&self, url: &Url) -> impl std::future::Future<Output = Result<Response>> + Send;
}
impl Transport for Fetcher {
    async fn get(&self, url: &Url) -> Result<Response> {
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none(),
            "invalid fetch URL"
        );
        let host = url.host_str().context("missing host")?;
        let port = url.port_or_known_default().context("missing port")?;
        let ips: Vec<SocketAddr> = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await??
        .collect();
        ensure!(
            !ips.is_empty() && ips.iter().all(|a| public_ip(a.ip())),
            "nonpublic address denied"
        );
        // Pin the checked addresses for this request; redirects are handled by the crawler.
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .resolve_to_addrs(host, &ips)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .user_agent(&self.user_agent)
            .build()?;
        let started = Instant::now();
        let mut response = client.get(url.clone()).send().await?;
        let ttfb_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let status = response.status().as_u16();
        let (headers, local_http_occurrences) = extract_headers(response.headers());
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            bail!("response too large");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                body.len().saturating_add(chunk.len()) <= MAX_BODY,
                "response too large"
            );
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status,
            headers,
            local_http_occurrences: Some(local_http_occurrences),
            body,
            ttfb_ms,
        })
    }
}
/// Extract the ordinary merged headers and separately retain bounded directive fields.
pub fn extract_headers(
    headers: &reqwest::header::HeaderMap,
) -> (
    BTreeMap<String, String>,
    crate::collection_evidence::Collection,
) {
    let mut merged = BTreeMap::<String, String>::new();
    let mut fields = Vec::new();
    let mut invalid = 0;
    for (key, value) in headers {
        if let Ok(value) = value.to_str() {
            merged
                .entry(key.to_string())
                .and_modify(|old| {
                    old.push_str(", ");
                    old.push_str(value);
                })
                .or_insert_with(|| value.to_owned());
            fields.push((key.as_str(), value));
        } else if key.as_str().eq_ignore_ascii_case("x-robots-tag")
            || key.as_str().eq_ignore_ascii_case("link")
        {
            invalid += 1;
        }
    }
    let mut occurrences = crate::collection_evidence::http_occurrences(&fields);
    if invalid > 0 {
        occurrences.complete = false;
        occurrences.omitted += invalid + occurrences.occurrences.len();
        occurrences.occurrences.clear();
    }
    (merged, occurrences)
}
pub fn service_url(value: &str, allow_private_http: bool) -> Result<Url> {
    let u = Url::parse(value)?;
    ensure!(
        u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "invalid service URL"
    );
    ensure!(
        u.scheme() == "https" || (allow_private_http && u.scheme() == "http"),
        "HTTPS required"
    );
    Ok(u)
}

#[cfg(test)]
mod header_tests {
    use super::*;
    #[test]
    fn ordinary_extraction_retains_repeated_and_marks_invalid_or_limited() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.append("x-robots-tag", HeaderValue::from_static("index"));
        h.append("x-robots-tag", HeaderValue::from_static("noindex"));
        let (ordinary, evidence) = extract_headers(&h);
        assert_eq!(ordinary["x-robots-tag"], "index, noindex");
        assert_eq!(evidence.occurrences.len(), 2);
        assert_eq!(evidence.occurrences[1].ordinal, 1);
        h.append("x-robots-tag", HeaderValue::from_bytes(&[0xff]).unwrap());
        let (ordinary, evidence) = extract_headers(&h);
        assert_eq!(ordinary["x-robots-tag"], "index, noindex");
        assert!(!evidence.complete);
        assert!(evidence.occurrences.is_empty());
        assert_eq!(evidence.omitted, 3);
        let mut h = HeaderMap::new();
        h.append(
            "x-robots-tag",
            HeaderValue::from_str(&"x".repeat(16_385)).unwrap(),
        );
        let (_, evidence) = extract_headers(&h);
        assert!(!evidence.complete);
        assert_eq!(evidence.omitted, 1);
    }
}
