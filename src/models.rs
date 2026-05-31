//! Shared data types: raw activity from the Data-API and the copy order we derive.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn parse(s: &str) -> Option<Side> {
        match s.to_ascii_uppercase().as_str() {
            "BUY" => Some(Side::Buy),
            "SELL" => Some(Side::Sell),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }

    /// EIP-712 / CLOB enum value: 0 = BUY, 1 = SELL.
    pub fn as_u8(&self) -> u8 {
        match self {
            Side::Buy => 0,
            Side::Sell => 1,
        }
    }
}

/// One activity item as returned by `GET {data_api}/activity?user=...`.
/// Polymarket returns camelCase JSON; unknown fields are ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
// Some fields are retained for completeness / future use even if unread today.
#[allow(dead_code)]
pub struct ActivityItem {
    #[serde(rename = "type", default)]
    pub activity_type: String,
    #[serde(default)]
    pub side: Option<String>,
    /// Outcome shares traded.
    #[serde(default)]
    pub size: f64,
    /// USDC notional, when provided by the API.
    #[serde(default)]
    pub usdc_size: Option<f64>,
    /// Fill price in [0, 1].
    #[serde(default)]
    pub price: f64,
    /// ERC-1155 token id of the traded outcome (what the CLOB calls `tokenId`).
    #[serde(default)]
    pub asset: String,
    #[serde(default)]
    pub condition_id: String,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub outcome_index: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub transaction_hash: Option<String>,
    /// The trader's wallet (proxy) address.
    #[serde(default)]
    pub proxy_wallet: Option<String>,
}

impl ActivityItem {
    pub fn is_trade(&self) -> bool {
        self.activity_type.eq_ignore_ascii_case("TRADE")
    }

    pub fn side_enum(&self) -> Option<Side> {
        self.side.as_deref().and_then(Side::parse)
    }

    /// Stable dedup key. A single tx can contain several fills, so we mix in the
    /// asset, side and size as well as the tx hash and timestamp.
    pub fn dedup_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.timestamp,
            self.transaction_hash.as_deref().unwrap_or("notx"),
            self.asset,
            self.side.as_deref().unwrap_or("?"),
            self.size,
        )
    }
}

/// A concrete order we intend to place to mirror a target's trade.
#[derive(Debug, Clone, Serialize)]
pub struct CopyOrder {
    pub token_id: String,
    pub side: Side,
    /// Limit price actually submitted (target price adjusted for slippage).
    pub price: f64,
    /// Reference price (the target's fill price), kept for logging.
    pub ref_price: f64,
    pub size_shares: f64,
    pub usdc: f64,
    // Context for logging / the ledger:
    pub condition_id: String,
    pub outcome: Option<String>,
    pub title: Option<String>,
    pub target: String,
    pub target_label: String,
    pub source_key: String,
}
