//! Minimal authenticated REST client for the Polymarket CLOB `/order` endpoint.

use super::auth::{build_headers, L2Creds};
use super::signing::SignedOrderPayload;
use anyhow::{bail, Context, Result};
use reqwest::header::CONTENT_TYPE;
use reqwest::Client;

pub struct ClobClient {
    http: Client,
    base: String,
    creds: L2Creds,
}

impl ClobClient {
    pub fn new(http: Client, base: String, creds: L2Creds) -> ClobClient {
        ClobClient {
            http,
            base: base.trim_end_matches('/').to_string(),
            creds,
        }
    }

    /// Submit a signed order. `order_type` is typically "GTC" (resting) or
    /// "FOK"/"FAK" (immediate). Returns the raw response body on success.
    pub async fn post_order(
        &self,
        payload: &SignedOrderPayload,
        order_type: &str,
    ) -> Result<String> {
        let body_value = serde_json::json!({
            "order": payload,
            "owner": self.creds.api_key,
            "orderType": order_type,
        });
        let body = serde_json::to_string(&body_value)?;
        let timestamp = chrono::Utc::now().timestamp();
        let headers = build_headers(&self.creds, "POST", "/order", &body, timestamp)?;

        let mut req = self
            .http
            .post(format!("{}/order", self.base))
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        for (k, v) in headers {
            req = req.header(k, v);
        }

        let resp = req.send().await.context("posting order to CLOB")?;
        let status = resp.status();
        let text = resp.text().await.context("reading CLOB response")?;
        if !status.is_success() {
            bail!("CLOB /order returned {status}: {text}");
        }
        Ok(text)
    }
}
