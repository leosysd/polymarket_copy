//! Polymarket CLOB L2 authentication headers (HMAC-SHA256), matching
//! `py-clob-client`'s `build_hmac_signature`.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct L2Creds {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
    /// The signing EOA address (checksummed) — sent as POLY_ADDRESS.
    pub address: String,
}

/// Build the five POLY_* headers for an authenticated request.
/// `signed message = timestamp + method + request_path + body`.
pub fn build_headers(
    creds: &L2Creds,
    method: &str,
    request_path: &str,
    body: &str,
    timestamp: i64,
) -> Result<Vec<(&'static str, String)>> {
    let message = format!("{timestamp}{method}{request_path}{body}");
    let secret = URL_SAFE
        .decode(creds.secret.as_bytes())
        .context("base64-decoding PM_API_SECRET")?;
    let mut mac = HmacSha256::new_from_slice(&secret).context("initialising HMAC")?;
    mac.update(message.as_bytes());
    let signature = URL_SAFE.encode(mac.finalize().into_bytes());

    Ok(vec![
        ("POLY_ADDRESS", creds.address.clone()),
        ("POLY_SIGNATURE", signature),
        ("POLY_TIMESTAMP", timestamp.to_string()),
        ("POLY_API_KEY", creds.api_key.clone()),
        ("POLY_PASSPHRASE", creds.passphrase.clone()),
    ])
}
