//! Deliver one report body to an HTP receiver once and print the outcome.
//!
//! Used by the cross-repo e2e (negative case: a mutated report must be refused
//! with 422). Usage: `deliver_once <endpoint> <report.json>`, token in
//! `HTP_INGEST_TOKEN`. Prints `accepted`, `terminal <status>` or `retryable <status>`.
use anyhow::{Context, Result};
use seosmith::{
    delivery::{Delivery, HtpStatus, HtpTerminal},
    fetch::service_url,
    model::digest,
};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let endpoint = args
        .next()
        .context("usage: deliver_once <endpoint> <report.json>")?;
    let path = args
        .next()
        .context("usage: deliver_once <endpoint> <report.json>")?;
    let token = std::env::var("HTP_INGEST_TOKEN").context("HTP_INGEST_TOKEN is required")?;
    let body = std::fs::read_to_string(path)?;
    let sha = digest(body.as_bytes());
    // Plain HTTP is allowed to ANY host here (a dev tool for the local e2e; never shipped,
    // and not subject to the serve-path loopback-literal gate). Use it only against local receivers.
    let delivery = Delivery::new(service_url(&endpoint, true)?, token)?;
    let id = uuid::Uuid::new_v4().to_string();
    match delivery.send(&id, body, &sha).await {
        Ok(()) => println!("accepted"),
        Err(error) => {
            if let Some(HtpTerminal(status)) = error.downcast_ref() {
                println!("terminal {}", status.as_u16());
            } else if let Some(HtpStatus(status)) = error.downcast_ref() {
                println!("retryable {}", status.as_u16());
            } else {
                println!("retryable transport");
            }
        }
    }
    Ok(())
}
