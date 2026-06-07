//! Turns a target's fill into the order we'll place, scaled proportionally to
//! the target's size and clamped by the USDC limits.

use crate::config::{FileConfig, OrderStyle, Target};
use crate::models::{CopyOrder, Side, TargetTrade};

/// Reasons a trade is intentionally not copied (logged, not errors).
#[derive(Debug)]
pub enum Skip {
    ExitFilteredOut,
    BadPrice,
    BelowMin { usdc: f64 },
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Skip::ExitFilteredOut => write!(f, "SELL skipped (only_buys)"),
            Skip::BadPrice => write!(f, "price out of (0,1) range"),
            Skip::BelowMin { usdc } => write!(f, "below min_order_usdc ({usdc:.2} USDC)"),
        }
    }
}

/// Build a copy order from a target trade, or explain why we're skipping it.
pub fn build_order(
    trade: &TargetTrade,
    target: &Target,
    cfg: &FileConfig,
) -> Result<CopyOrder, Skip> {
    if cfg.only_buys && trade.side == Side::Sell {
        return Err(Skip::ExitFilteredOut);
    }
    if trade.price <= 0.0 || trade.price >= 1.0 {
        return Err(Skip::BadPrice);
    }

    // Proportional sizing: copy a fraction of the target's share count.
    //   our_shares = target_shares * copy_factor * weight
    // The executor always receives a concrete share size plus a submitted price.
    // Market mode uses that price as a taker cap/floor; maker mode uses it as a
    // post-only limit placed away from the opposing book.
    let order_price = priced_for_style(cfg.order_style, trade.side, trade.price, cfg.max_slippage);
    let cap_price = order_price;
    let mut shares = trade.shares * cfg.copy_factor * target.weight;
    let usdc = shares * cap_price;

    // Per-copy USDC ceiling / dust floor.
    if usdc > cfg.max_order_usdc {
        shares = floor_shares(cfg.max_order_usdc / cap_price);
    }

    let mut shares = round_shares(shares);
    let mut usdc = shares * cap_price;
    if usdc > cfg.max_order_usdc {
        shares = floor_shares(cfg.max_order_usdc / cap_price);
        usdc = shares * cap_price;
    }
    if shares <= 0.0 || usdc < cfg.min_order_usdc {
        return Err(Skip::BelowMin { usdc });
    }

    Ok(CopyOrder {
        token_id: trade.token_id.clone(),
        side: trade.side,
        price: round_price_for_order(cfg.order_style, trade.side, order_price),
        ref_price: trade.price,
        size_shares: shares,
        usdc,
        target: trade.target.clone(),
        target_label: target.label.clone().unwrap_or_else(|| target.address.clone()),
        source_key: trade.dedup_key(),
    })
}

/// Polymarket prices tick in cents (2 decimals). Keep maker orders passive when
/// quantizing: BUY rounds down, SELL rounds up.
fn round_price_for_order(style: OrderStyle, side: Side, p: f64) -> f64 {
    let cents = p * 100.0;
    let rounded = match (style, side) {
        (OrderStyle::Maker, Side::Buy) => cents.floor() / 100.0,
        (OrderStyle::Maker, Side::Sell) => cents.ceil() / 100.0,
        (OrderStyle::Market, Side::Buy) => cents.ceil() / 100.0,
        (OrderStyle::Market, Side::Sell) => cents.floor() / 100.0,
    };
    rounded.clamp(0.01, 0.99)
}

/// Shares to 2 decimals — plenty for CLOB minimum increments.
fn round_shares(s: f64) -> f64 {
    (s * 100.0).round() / 100.0
}

fn floor_shares(s: f64) -> f64 {
    (s * 100.0).floor() / 100.0
}

fn priced_for_style(style: OrderStyle, side: Side, price: f64, max_slippage: f64) -> f64 {
    match (style, side) {
        (OrderStyle::Market, Side::Buy) => (price + max_slippage).clamp(0.01, 0.99),
        (OrderStyle::Market, Side::Sell) => (price - max_slippage).clamp(0.01, 0.99),
        (OrderStyle::Maker, Side::Buy) => (price - max_slippage).clamp(0.01, 0.99),
        (OrderStyle::Maker, Side::Sell) => (price + max_slippage).clamp(0.01, 0.99),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Endpoints, StatePaths};
    use std::time::Instant;

    fn cfg() -> FileConfig {
        FileConfig {
            mode: crate::config::Mode::DryRun,
            copy_factor: 0.25,
            min_order_usdc: 1.0,
            max_order_usdc: 50.0,
            only_buys: true,
            align_to_window: false,
            max_slippage: 0.02,
            order_style: crate::config::OrderStyle::Market,
            order_type: "FAK".to_string(),
            aggregate_window_ms: 0,
            max_market_usdc: 0.0,
            market_filter: String::new(),
            targets: Vec::new(),
            endpoints: Endpoints {
                clob: "https://clob.polymarket.com".to_string(),
                chain_id: 137,
                exchange: "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E".to_string(),
                gamma: "https://gamma-api.polymarket.com".to_string(),
                log_sources: Vec::new(),
            },
            state: StatePaths {
                state_file: "data/state.json".to_string(),
                ledger_file: "data/copies.jsonl".to_string(),
            },
        }
    }

    fn target() -> Target {
        Target {
            address: "0xe0229e10a858860218b6132f4234602c47bd6603".to_string(),
            weight: 1.0,
            label: Some("target".to_string()),
        }
    }

    fn trade(side: Side, price: f64, shares: f64) -> TargetTrade {
        TargetTrade {
            target: "0xe0229e10a858860218b6132f4234602c47bd6603".to_string(),
            side,
            token_id: "123".to_string(),
            price,
            shares,
            usdc: price * shares,
            tx_hash: "0xabc".to_string(),
            log_index: 1,
            received_at: Instant::now(),
            recv_unix_ms: 0,
            block_time: None,
        }
    }

    #[test]
    fn buy_copies_target_shares_with_price_cap() {
        let order = build_order(&trade(Side::Buy, 0.52, 20.0), &target(), &cfg()).unwrap();
        assert_eq!(order.size_shares, 5.0);
        assert_eq!(order.price, 0.54);
        assert_eq!(order.ref_price, 0.52);
    }

    #[test]
    fn max_order_usdc_caps_worst_case_buy_notional() {
        let mut cfg = cfg();
        cfg.max_order_usdc = 2.0;
        let order = build_order(&trade(Side::Buy, 0.52, 20.0), &target(), &cfg).unwrap();
        assert!(order.usdc <= 2.0 + f64::EPSILON);
    }

    #[test]
    fn max_order_usdc_caps_after_share_rounding() {
        let mut cfg = cfg();
        cfg.min_order_usdc = 0.0;
        cfg.max_order_usdc = 1.0;
        let order = build_order(&trade(Side::Buy, 0.822, 20.0), &target(), &cfg).unwrap();
        assert_eq!(order.size_shares, 1.18);
        assert!(order.usdc <= 1.0 + f64::EPSILON);
    }

    #[test]
    fn sell_uses_price_floor() {
        let mut cfg = cfg();
        cfg.only_buys = false;
        let order = build_order(&trade(Side::Sell, 0.52, 20.0), &target(), &cfg).unwrap();
        assert_eq!(order.size_shares, 5.0);
        assert_eq!(order.price, 0.5);
    }

    #[test]
    fn maker_uses_passive_limit_offset() {
        let mut cfg = cfg();
        cfg.order_style = crate::config::OrderStyle::Maker;

        let buy = build_order(&trade(Side::Buy, 0.52, 20.0), &target(), &cfg).unwrap();
        assert_eq!(buy.size_shares, 5.0);
        assert_eq!(buy.price, 0.5);

        cfg.only_buys = false;
        let sell = build_order(&trade(Side::Sell, 0.52, 20.0), &target(), &cfg).unwrap();
        assert_eq!(sell.size_shares, 5.0);
        assert_eq!(sell.price, 0.54);
    }

    #[test]
    fn maker_prices_quantize_to_cents_passively() {
        let mut cfg = cfg();
        cfg.order_style = crate::config::OrderStyle::Maker;
        cfg.max_slippage = 0.0;

        let buy = build_order(&trade(Side::Buy, 0.3457, 20.0), &target(), &cfg).unwrap();
        assert_eq!(buy.price, 0.34);

        cfg.only_buys = false;
        let sell = build_order(&trade(Side::Sell, 0.3457, 20.0), &target(), &cfg).unwrap();
        assert_eq!(sell.price, 0.35);
    }
}
