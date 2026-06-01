//! Low-latency monitor for target wallets' Polymarket fills.
//!
//! Primary path: **`eth_subscribe` logs** over a WebSocket — the moment a
//! matching `OrderFilled` is pushed, it triggers a copy (lowest latency).
//!
//! Safety net: **`eth_getLogs`** backfill, because subscription delivery can
//! silently drop logs on some nodes. We getLogs:
//!   - on startup (recent N blocks),
//!   - on every reconnect (last_seen_block → latest, closing the gap),
//!   - periodically (calibration sweep).
//! Both paths feed the same channel; the consumer dedups by (tx, log_index),
//! so a fill is copied once — as soon as either path sees it.
//!
//! The Polymarket exchange that settles these markets (`0xe1111800…`) emits a
//! fill event with topic0 `ORDER_FILLED_TOPIC` and data layout:
//!   topics: [sig, orderHash, maker, taker]
//!   data:   [makerAssetId, takerAssetId, makerAmountFilled, takerAmountFilled, fee]
//! Collateral (USDC) is asset id 0; the other side is the CTF token id.

use crate::models::{Side, TargetTrade};
use alloy::primitives::{b256, Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::{Filter, Log};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// keccak topic0 of the Polymarket fill event (verified on-chain against live
/// BTC 5-minute trades; the contract is a custom deploy, not the public ABI).
const ORDER_FILLED_TOPIC: B256 =
    b256!("d543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee");

/// How long to wait before reconnecting after the connection drops.
const RECONNECT_DELAY: Duration = Duration::from_millis(100);
/// Periodic getLogs calibration sweep (safety net for dropped subscription logs).
const CALIBRATE_INTERVAL: Duration = Duration::from_secs(5);
/// On first startup, backfill this many recent blocks (covers the tiny gap
/// before the subscription becomes active). The window-alignment gate prevents
/// copying anything stale anyway.
const STARTUP_LOOKBACK_BLOCKS: u64 = 3;

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

    /// Spawn the monitor on a background task. Decoded trades arrive on the
    /// returned channel; the loop reconnects automatically and keeps the last
    /// processed block so reconnect gaps get backfilled.
    pub fn spawn(self) -> mpsc::Receiver<TargetTrade> {
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let mut last_block: Option<u64> = None;
            loop {
                match self.run_once(&tx, &mut last_block).await {
                    Err(e) => warn!(error = %e, "monitor connection error; reconnecting"),
                    Ok(()) => warn!("monitor stream ended; reconnecting"),
                }
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        });
        rx
    }

    fn filter(&self) -> Filter {
        let maker_topics: Vec<B256> = self.targets.iter().map(|a| a.into_word()).collect();
        Filter::new()
            .address(self.sources.clone())
            .event_signature(ORDER_FILLED_TOPIC)
            .topic2(maker_topics)
    }

    /// getLogs over [from, to] and forward decoded trades. Deduped downstream.
    /// Returns false if the channel closed.
    async fn backfill<P: Provider>(
        &self,
        provider: &P,
        from: u64,
        to: u64,
        tx: &mpsc::Sender<TargetTrade>,
    ) -> Result<bool> {
        if from > to {
            return Ok(true);
        }
        let filter = self.filter().from_block(from).to_block(to);
        let logs = provider.get_logs(&filter).await.context("get_logs backfill")?;
        for log in &logs {
            if let Some(trade) = decode(log) {
                if tx.send(trade).await.is_err() {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    async fn run_once(
        &self,
        tx: &mpsc::Sender<TargetTrade>,
        last_block: &mut Option<u64>,
    ) -> Result<()> {
        let provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(self.wss_url.clone()))
            .await
            .context("connecting to Polygon WebSocket RPC")?;

        // (Re)connect backfill: from last seen block (reconnect gap) or a small
        // startup lookback. Closes any window where the subscription was down.
        let head = provider.get_block_number().await.context("get_block_number")?;
        let from = match *last_block {
            Some(b) => b + 1,
            None => head.saturating_sub(STARTUP_LOOKBACK_BLOCKS),
        };
        info!(
            targets = self.targets.len(),
            sources = self.sources.len(),
            from_block = from,
            head,
            "monitor: eth_subscribe (primary) + eth_getLogs (backfill)"
        );
        if !self.backfill(&provider, from, head, tx).await? {
            return Ok(());
        }
        *last_block = Some(head);

        // Primary low-latency path: subscription push.
        let sub = provider
            .subscribe_logs(&self.filter())
            .await
            .context("subscribing to order-fill logs")?;
        let mut stream = sub.into_stream();
        let mut ticker = tokio::time::interval(CALIBRATE_INTERVAL);
        ticker.tick().await; // drop the immediate first tick

        loop {
            tokio::select! {
                maybe_log = stream.next() => {
                    match maybe_log {
                        Some(log) => {
                            if let Some(trade) = decode(&log) {
                                if tx.send(trade).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        None => return Ok(()), // stream ended -> reconnect (+ gap backfill)
                    }
                }
                _ = ticker.tick() => {
                    // Calibration sweep: backfill anything the subscription missed.
                    let head = provider.get_block_number().await.context("get_block_number")?;
                    let from = last_block.map(|b| b + 1).unwrap_or(head);
                    if !self.backfill(&provider, from, head, tx).await? {
                        return Ok(());
                    }
                    *last_block = Some(head);
                }
            }
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
