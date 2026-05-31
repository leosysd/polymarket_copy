//! Polls the Polymarket Data-API for each target's recent trade activity.

use crate::models::ActivityItem;
use anyhow::{Context, Result};
use reqwest::Client;

pub struct Monitor {
    client: Client,
    data_api: String,
    limit: u32,
}

impl Monitor {
    pub fn new(client: Client, data_api: String) -> Monitor {
        Monitor {
            client,
            data_api: data_api.trim_end_matches('/').to_string(),
            limit: 100,
        }
    }

    /// Fetch the most recent activity for one trader, newest-first as the API
    /// returns it. Network/parse errors are surfaced so the caller can log and
    /// keep the loop alive.
    pub async fn fetch_activity(&self, address: &str) -> Result<Vec<ActivityItem>> {
        let url = format!("{}/activity", self.data_api);
        let resp = self
            .client
            .get(&url)
            .query(&[
                ("user", address),
                ("limit", &self.limit.to_string()),
                ("sortBy", "TIMESTAMP"),
                ("sortDirection", "DESC"),
            ])
            .send()
            .await
            .with_context(|| format!("requesting activity for {address}"))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .with_context(|| format!("reading activity body for {address}"))?;

        if !status.is_success() {
            anyhow::bail!("data-api returned {status} for {address}: {body}");
        }

        let items: Vec<ActivityItem> = serde_json::from_str(&body)
            .with_context(|| format!("parsing activity JSON for {address}"))?;
        Ok(items)
    }
}
