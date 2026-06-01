//! Low-latency monitor: polls Polygon for the target wallets' order-fill logs
//! via `eth_getLogs`, block by block, and decodes them in near real-time.
//!
//! We poll `eth_getLogs` rather than `eth_subscribe` because subscription
//! delivery proved unreliable on real nodes (it silently dropped matching logs);
//! `eth_getLogs` is the node's authoritative state with no cache, so it never
//! misses a fill. Latency is still ~block time (a few seconds), not the 1–3 min
//! of the Data-API.
//!
//! The Polymarket exchange that settles these markets
//! (`0xe1111800…`) emits a fill event whose topic0 is the hardcoded
//! `ORDER_FILLED_TOPIC` below and whose data layout is:
//!   topics: [sig, orderHash, maker, taker]
//!   data:   [makerAssetId, takerAssetId, makerAmountFilled, takerAmountFilled, fee]
//! Collateral (USDC) is asset id 0; the other side is the CTF token id.

use crate::models::{Side, TargetTrade};
use alloy::primitives::{b256, Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::{Filter, Log};
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// keccak topic0 of the Polymarket fill event (verified on-chain against live
/// BTC 5-minute trades; the contract is a custom deploy, not the public ABI).
const ORDER_FILLED_TOPIC: B256 =
    b256!("d543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee");

/// How long to wait before reconnecting after the RPC connection drops.
const RECONNECT_DELAY: Duration = Duration::from_millis(100);

/// How often to poll for new blocks' logs. Polygon blocks are ~2s.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

pub struct ChainMonitor {
    wss_url: String,
    sources: Vec<Address>,
    targets: Vec<Address>,
}

impl ChainMonitor {
    pub fn new(wss_url: String, sources: Vec<Address>, targets: Vec<Address>) -> ChainMonitor {
        ChainMonitor {
            wss_url,
            sources,
            targets,
        }
    }

    /// Spawn the subscription loop on a background task. Decoded trades arrive on
    /// the returned channel; the loop reconnects automatically on disconnect.
    pub fn spawn(self) -> mpsc::Receiver<TargetTrade> {
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                if let Err(e) = self.run_once(&tx).await {
                    warn!(error = %e, "log subscription dropped; reconnecting");
                } else {
                    warn!("log subscription ended; reconnecting");
                }
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        });
        rx
    }

    async fn run_once(&self, tx: &mpsc::Sender<TargetTrade>) -> Result<()> {
        let provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(self.wss_url.clone()))
            .await
            .context("connecting to Polygon RPC")?;

        let maker_topics: Vec<B256> = self.targets.iter().map(|a| a.into_word()).collect();

        // Start from the current head; we only act on fills from now on (no replay).
        let mut last = provider
            .get_block_number()
            .await
            .context("get_block_number")?;
        info!(
            targets = self.targets.len(),
            sources = self.sources.len(),
            from_block = last,
            "polling on-chain fills (eth_getLogs)"
        );

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let latest = provider
                .get_block_number()
                .await
                .context("get_block_number")?;
            if latest <= last {
                continue;
            }

            // Fill events on the exchange(s) where maker (topic2) is a target.
            let filter = Filter::new()
                .address(self.sources.clone())
                .event_signature(ORDER_FILLED_TOPIC)
                .topic2(maker_topics.clone())
                .from_block(last + 1)
                .to_block(latest);

            let logs = provider.get_logs(&filter).await.context("get_logs")?;
            for log in &logs {
                if let Some(trade) = decode(log) {
                    if tx.send(trade).await.is_err() {
                        return Ok(());
                    }
                }
            }
            last = latest;
        }
    }
}

fn decode(log: &Log) -> Option<TargetTrade> {
    let topics = log.topics();
    if topics.first() != Some(&ORDER_FILLED_TOPIC) || topics.len() < 4 {
        return None;
    }
    let maker = Address::from_word(topics[2]);

    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return None;
    }
    let maker_asset = U256::from_be_slice(&data[0..32]);
    let taker_asset = U256::from_be_slice(&data[32..64]);
    let maker_amount = U256::from_be_slice(&data[64..96]);
    let taker_amount = U256::from_be_slice(&data[96..128]);

    let (side, token_id, shares, usdc) = if maker_asset == U256::ZERO {
        // Maker gave USDC, received shares -> the target BOUGHT.
        (
            Side::Buy,
            taker_asset,
            to_f64_6(taker_amount),
            to_f64_6(maker_amount),
        )
    } else {
        // Maker gave shares, received USDC -> the target SOLD.
        (
            Side::Sell,
            maker_asset,
            to_f64_6(maker_amount),
            to_f64_6(taker_amount),
        )
    };

    if shares <= 0.0 || usdc <= 0.0 {
        return None;
    }

    Some(TargetTrade {
        target: maker.to_checksum(None),
        side,
        token_id: token_id.to_string(),
        price: usdc / shares,
        shares,
        usdc,
        tx_hash: log
            .transaction_hash
            .map(|h| h.to_string())
            .unwrap_or_default(),
        log_index: log.log_index.unwrap_or_default(),
        received_at: std::time::Instant::now(),
    })
}

/// USDC and CTF tokens both use 6 decimals.
fn to_f64_6(v: U256) -> f64 {
    let micro: u128 = v.try_into().unwrap_or(u128::MAX);
    micro as f64 / 1_000_000.0
}
