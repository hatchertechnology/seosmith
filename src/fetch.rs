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
            user_agent: "Seosmith/0.1 (+https://github.com/hatchertechnology/seosmith)".into(),
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
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_owned())))
            .collect();
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
            body,
            ttfb_ms,
        })
    }
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
