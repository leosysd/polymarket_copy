//! Derive (or create) Polymarket CLOB API credentials from a private key, so
//! live mode never needs `py-clob-client`. Uses L1 (ClobAuth) authentication.

use super::signing::OrderSigner;
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;

/// API credentials returned by `/auth/api-key` or `/auth/derive-api-key`.
#[derive(Debug, Clone, Deserialize)]
pub struct DerivedCreds {
    #[serde(alias = "apiKey")]
    pub api_key: String,
    #[serde(alias = "api_secret")]
    pub secret: String,
    #[serde(alias = "api_passphrase")]
    pub passphrase: String,
}

fn l1_headers(signer: &OrderSigner, nonce: u64) -> Result<Vec<(&'static str, String)>> {
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let signature = signer.sign_clob_auth(&timestamp, nonce)?;
    Ok(vec![
        ("POLY_ADDRESS", signer.address().to_checksum(None)),
        ("POLY_SIGNATURE", signature),
        ("POLY_TIMESTAMP", timestamp),
        ("POLY_NONCE", nonce.to_string()),
    ])
}

async fn call(
    http: &Client,
    url: &str,
    method: &str,
    signer: &OrderSigner,
    nonce: u64,
) -> Result<DerivedCreds> {
    let headers = l1_headers(signer, nonce)?;
    let mut req = match method {
        "POST" => http.post(url),
        _ => http.get(url),
    };
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send().await.with_context(|| format!("requesting {url}"))?;
    let status = resp.status();
    let text = resp.text().await.context("reading creds response")?;
    if !status.is_success() {
        bail!("{method} {url} -> {status}: {text}");
    }
    serde_json::from_str(&text).with_context(|| format!("parsing creds JSON: {text}"))
}

/// Try to create a fresh API key; if one already exists, derive it. Both are
/// deterministic for a given (address, nonce), so this is idempotent.
pub async fn create_or_derive_api_creds(
    http: &Client,
    clob_base: &str,
    signer: &OrderSigner,
    nonce: u64,
) -> Result<DerivedCreds> {
    let base = clob_base.trim_end_matches('/');
    match call(http, &format!("{base}/auth/api-key"), "POST", signer, nonce).await {
        Ok(creds) => Ok(creds),
        Err(create_err) => {
            // Key likely already exists — fall back to deriving it.
            call(
                http,
                &format!("{base}/auth/derive-api-key"),
                "GET",
                signer,
                nonce,
            )
            .await
            .with_context(|| format!("create failed ({create_err}); derive also failed"))
        }
    }
}
