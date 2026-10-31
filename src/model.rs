use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RULESET_VERSION: &str = "2026-10-31.1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CrawlRequest {
    pub request_id: Uuid,
    pub website_id: Uuid,
    pub binding_generation: u64,
    pub url: Url,
    #[serde(default = "default_pages")]
    pub max_pages: usize,
    #[serde(default = "default_duration")]
    pub max_duration_seconds: u64,
    #[serde(default = "default_delay")]
    pub delay_ms: u64,
    #[serde(default)]
    pub check_external_links: bool,
}
const fn default_pages() -> usize {
    500
}
const fn default_duration() -> u64 {
    1800
}
const fn default_delay() -> u64 {
    1000
}
impl CrawlRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.url.scheme(), "https" | "http"),
            "HTTP(S) required"
        );
        ensure!(
            self.url.username().is_empty() && self.url.password().is_none(),
            "URL credentials forbidden"
        );
        ensure!(
            self.url.host_str().is_some() && self.url.port().is_none(),
            "host required; custom ports forbidden"
        );
        ensure!(
            self.url.fragment().is_none() && self.url.query().is_none(),
            "seed cannot contain query or fragment"
        );
        ensure!(
            (1..=20_000).contains(&self.max_pages),
            "max_pages must be 1..20000"
        );
        ensure!(
            (10..=7200).contains(&self.max_duration_seconds),
            "duration must be 10..7200 seconds"
        );
        ensure!(
            (1000..=60_000).contains(&self.delay_ms),
            "delay must be 1000..60000 ms"
        );
        Ok(())
    }
    pub fn in_scope(&self, u: &Url) -> bool {
        matches!(u.scheme(), "https" | "http")
            && u.host_str() == self.url.host_str()
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Link {
    pub url: String,
    pub nofollow: bool,
    pub text: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Image {
    pub url: String,
    pub alt: Option<String>,
    pub width: bool,
    pub height: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Alternate {
    pub language: String,
    pub url: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Page {
    pub url: String,
    pub depth: usize,
    pub in_sitemap: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub blocked_by_robots: bool,
    pub content_type: String,
    pub bytes: usize,
    pub ttfb_ms: u64,
    pub title: String,
    pub description: String,
    pub language: String,
    pub robots: String,
    pub canonical: Option<String>,
    pub header_canonical: Option<String>,
    pub redirect: Option<String>,
    pub headings: Vec<(u8, String)>,
    pub links: Vec<Link>,
    pub images: Vec<Image>,
    pub alternates: Vec<Alternate>,
    pub body_hash: String,
    pub words: usize,
    pub dom_nodes: usize,
    pub flags: Vec<String>,
}
impl Page {
    pub fn html(&self) -> bool {
        self.status == Some(200) && self.content_type.to_ascii_lowercase().contains("text/html")
    }
    pub fn noindex(&self) -> bool {
        self.robots
            .split([',', ' ', ';'])
            .any(|x| x.eq_ignore_ascii_case("noindex") || x.eq_ignore_ascii_case("none"))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Alert,
    Warning,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    pub key: String,
    pub rule: String,
    pub severity: Severity,
    pub url: String,
    pub evidence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub engine_version: String,
    pub ruleset_version: String,
    pub request: CrawlRequest,
    pub started_at: String,
    pub finished_at: String,
    pub outcome: String,
    pub stop_reason: String,
    pub coverage_complete: bool,
    pub pages: Vec<Page>,
    pub findings: Vec<Finding>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryEnvelope {
    pub delivery_id: Uuid,
    pub sha256: String,
    pub report_json: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    pub delivery_id: Uuid,
    pub sha256: String,
    pub accepted: bool,
}
pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn normalize(base: &Url, value: &str) -> Option<Url> {
    let mut u = base.join(value.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some()
    {
        return None;
    }
    u.set_fragment(None);
    Some(u)
}
