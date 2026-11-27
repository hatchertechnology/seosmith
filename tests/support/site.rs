//! An in-memory origin for crawler tests: URL -> canned response, 404 otherwise.
#![allow(dead_code, clippy::unused_async_trait_impl)]
use anyhow::Result;
use seosmith::{
    fetch::{Response, Transport},
    model::CrawlRequest,
};
use std::collections::BTreeMap;
use url::Url;

type Canned = (u16, Vec<(&'static str, String)>, Vec<u8>);

#[derive(Default)]
pub struct Site(pub BTreeMap<String, Canned>);

impl Site {
    pub fn html(mut self, url: &str, body: &str) -> Self {
        self.0.insert(
            url.into(),
            (
                200,
                vec![("content-type", "text/html; charset=utf-8".into())],
                body.as_bytes().to_vec(),
            ),
        );
        self
    }
    pub fn raw(
        mut self,
        url: &str,
        status: u16,
        headers: &[(&'static str, &str)],
        body: &[u8],
    ) -> Self {
        self.0.insert(
            url.into(),
            (
                status,
                headers.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(),
                body.to_vec(),
            ),
        );
        self
    }
    pub fn robots(self, body: &str) -> Self {
        self.raw(
            "https://site.invalid/robots.txt",
            200,
            &[("content-type", "text/plain")],
            body.as_bytes(),
        )
    }
}

impl Transport for Site {
    async fn get(&self, url: &Url) -> Result<Response> {
        let (status, headers, body) =
            self.0
                .get(url.as_str())
                .cloned()
                .unwrap_or((404, Vec::new(), Vec::new()));
        Ok(Response {
            status,
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            local_http_occurrences: None,
            body,
            ttfb_ms: 1,
        })
    }
}

pub fn request(max_pages: usize, max_resources: usize) -> CrawlRequest {
    let report: seosmith::model::Report =
        serde_json::from_str(include_str!("../../fixtures/contracts/legacy.json")).unwrap();
    let mut r = report.request;
    r.request_id = uuid::Uuid::new_v4();
    r.url = "https://site.invalid/".parse().unwrap();
    r.max_pages = max_pages;
    r.max_resources = max_resources;
    r.max_duration_seconds = 60;
    r.delay_ms = 1000;
    r.check_external_links = false;
    r
}

/// The crawl loop sleeps `delay_ms` before every fetch; an in-memory origin needs no pacing.
pub fn unpaced(r: &CrawlRequest) -> CrawlRequest {
    CrawlRequest {
        delay_ms: 0,
        ..r.clone()
    }
}
