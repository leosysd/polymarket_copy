//! Turns a target's trade into the order we'll place, scaled proportionally to
//! the target's size and clamped by the USDC limits.

use crate::config::{FileConfig, Target};
use crate::models::{ActivityItem, CopyOrder, Side};

/// Reasons a trade is intentionally not copied (logged, not errors).
#[derive(Debug)]
pub enum Skip {
    NotATrade,
    UnknownSide,
    ExitFilteredOut,
    BadPrice,
    BelowMin { usdc: f64 },
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Skip::NotATrade => write!(f, "not a TRADE"),
            Skip::UnknownSide => write!(f, "unknown side"),
            Skip::ExitFilteredOut => write!(f, "SELL skipped (only_buys)"),
            Skip::BadPrice => write!(f, "price out of (0,1) range"),
            Skip::BelowMin { usdc } => write!(f, "below min_order_usdc ({usdc:.2} USDC)"),
        }
    }
}

/// Build a copy order from a target trade, or explain why we're skipping it.
pub fn build_order(
    item: &ActivityItem,
    target: &Target,
    cfg: &FileConfig,
) -> Result<CopyOrder, Skip> {
    if !item.is_trade() {
        return Err(Skip::NotATrade);
    }
    let side = item.side_enum().ok_or(Skip::UnknownSide)?;
    if cfg.only_buys && side == Side::Sell {
        return Err(Skip::ExitFilteredOut);
    }
    if item.price <= 0.0 || item.price >= 1.0 {
        return Err(Skip::BadPrice);
    }

    // Proportional sizing: scale the target's share count.
    let mut shares = item.size * cfg.copy_factor * target.weight;
    let mut usdc = shares * item.price;

    // Clamp to the per-copy USDC ceiling.
    if usdc > cfg.max_order_usdc {
        usdc = cfg.max_order_usdc;
        shares = usdc / item.price;
    }
    if usdc < cfg.min_order_usdc {
        return Err(Skip::BelowMin { usdc });
    }

    // Marketable-limit price: nudge so the order crosses the book.
    let slip = cfg.max_slippage_bps as f64 / 10_000.0;
    let limit_price = match side {
        Side::Buy => (item.price * (1.0 + slip)).min(0.999),
        Side::Sell => (item.price * (1.0 - slip)).max(0.001),
    };

    Ok(CopyOrder {
        token_id: item.asset.clone(),
        side,
        price: round_price(limit_price),
        ref_price: item.price,
        size_shares: round_shares(shares),
        usdc: round_shares(shares) * limit_price,
        condition_id: item.condition_id.clone(),
        outcome: item.outcome.clone(),
        title: item.title.clone(),
        target: target.address.clone(),
        target_label: target.label.clone().unwrap_or_else(|| target.address.clone()),
        source_key: item.dedup_key(),
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
