//! Turns a target's fill into the order we'll place, scaled proportionally to
//! the target's size and clamped by the USDC limits.

use crate::config::{FileConfig, Target};
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
    // The order is placed as a MARKET order (see executor): a BUY spends
    // `usdc` (= shares * target price) at the current market; a SELL sells
    // `size_shares`. We don't set a limit price — the market fills at market.
    let mut shares = trade.shares * cfg.copy_factor * target.weight;
    let mut usdc = shares * trade.price;

    // Per-copy USDC ceiling / dust floor.
    if usdc > cfg.max_order_usdc {
        usdc = cfg.max_order_usdc;
        shares = usdc / trade.price;
    }
    if usdc < cfg.min_order_usdc {
        return Err(Skip::BelowMin { usdc });
    }

    let shares = round_shares(shares);
    let usdc = shares * trade.price;

    Ok(CopyOrder {
        token_id: trade.token_id.clone(),
        side: trade.side,
        // Reference price only (for logging/ledger); a market order picks its own.
        price: round_price(trade.price),
        ref_price: trade.price,
        size_shares: shares,
        usdc,
        target: trade.target.clone(),
        target_label: target.label.clone().unwrap_or_else(|| target.address.clone()),
        source_key: trade.dedup_key(),
    })
}

/// Polymarket prices tick in cents (2 decimals).
fn round_price(p: f64) -> f64 {
    (p * 100.0).round() / 100.0
}

/// Shares to 2 decimals — plenty for CLOB minimum increments.
fn round_shares(s: f64) -> f64 {
    (s * 100.0).round() / 100.0
}
