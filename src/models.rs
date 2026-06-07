//! Shared data types: a decoded on-chain trade and the copy order we derive.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
}

/// A target wallet's fill, decoded from an on-chain `OrderFilled` event.
#[derive(Debug, Clone)]
pub struct TargetTrade {
    /// The target wallet (the order's maker).
    pub target: String,
    pub side: Side,
    /// CTF position id of the traded outcome (the CLOB `tokenId`, decimal string).
    pub token_id: String,
    /// Fill price in (0, 1), derived from USDC / shares.
    pub price: f64,
    /// Outcome shares filled.
    pub shares: f64,
    /// USDC notional filled.
    pub usdc: f64,
    pub tx_hash: String,
    pub log_index: u64,
    /// When we received/decoded this fill — used to measure our own latency.
    pub received_at: std::time::Instant,
    /// Wall-clock (unix ms) at decode — paired with `block_time` for the
    /// "chain → we received it" detection latency.
    pub recv_unix_ms: i64,
    /// Block timestamp (unix seconds) of the fill, if the RPC provider includes
    /// it on the log. None when absent (some providers omit it on subscriptions).
    pub block_time: Option<u64>,
}

impl TargetTrade {
    /// Stable dedup key: a fill is uniquely a (transaction, log-index) pair.
    pub fn dedup_key(&self) -> String {
        format!("{}:{}", self.tx_hash, self.log_index)
    }
}

/// A concrete order we intend to place to mirror a target's trade.
#[derive(Debug, Clone, Serialize)]
pub struct CopyOrder {
    pub token_id: String,
    pub side: Side,
    /// Submitted price: market cap/floor or maker post-only limit.
    pub price: f64,
    /// Reference price (the target's fill price), kept for logging.
    pub ref_price: f64,
    pub size_shares: f64,
    /// Estimated worst-case notional for this copy at the submitted cap/floor.
    pub usdc: f64,
    pub target: String,
    pub target_label: String,
    pub source_key: String,
}
