use crate::{
    model::{DeliveryEnvelope, DeliveryReceipt},
    store::Store,
};
use anyhow::{Result, ensure};
use reqwest::{Client, redirect::Policy};
use std::time::Duration;
use url::Url;
#[derive(Clone)]
pub struct Delivery {
    client: Client,
    endpoint: Url,
    token: String,
}
impl Delivery {
    pub fn new(endpoint: Url, token: String) -> Result<Self> {
        ensure!(
            token.len() >= 32,
            "HTP token must be at least 32 characters"
        );
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .build()?,
            endpoint,
            token,
        })
    }
    pub async fn send(&self, id: &str, body: String, sha: &str) -> Result<()> {
        let delivery_id = id.parse()?;
        let envelope = DeliveryEnvelope {
            delivery_id,
            sha256: sha.into(),
            report_json: body,
        };
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.token)
            .header("Idempotency-Key", id)
            .json(&envelope)
            .send()
            .await?;
        ensure!(response.status().is_success(), "HTP rejected delivery");
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(bytes.len() + chunk.len() <= 4096, "oversized receipt");
            bytes.extend_from_slice(&chunk);
        }
        let receipt: DeliveryReceipt = serde_json::from_slice(&bytes)?;
        ensure!(
            receipt.accepted && receipt.delivery_id == delivery_id && receipt.sha256 == sha,
            "HTP receipt mismatch"
        );
        Ok(())
    }
    pub async fn tick(&self, store: &Store) -> Result<bool> {
        let Some((id, body, sha, attempt)) = store.pending().await? else {
            return Ok(false);
        };
        if let Ok(()) = self.send(&id, body, &sha).await {
            store.delivered(&id, &sha).await?;
            tracing::info!(run_id=%id,"HTP delivery acknowledged");
        } else {
            store.retry(&id, attempt).await?;
            tracing::warn!(run_id=%id,"HTP delivery deferred");
        }
        Ok(true)
    }
}
