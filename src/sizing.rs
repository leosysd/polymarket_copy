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

    // Sizing: either a fixed share count (follow the target's direction only),
    // or proportional to the target's size.
    let (mut shares, mut usdc) = if cfg.fixed_shares > 0.0 {
        (cfg.fixed_shares, cfg.fixed_shares * trade.price)
    } else {
        let s = trade.shares * cfg.copy_factor * target.weight;
        (s, s * trade.price)
    };

    // Per-copy USDC ceiling. In fixed-share mode this only bites if a single
    // fixed order would exceed the cap; otherwise the fixed count is kept.
    if usdc > cfg.max_order_usdc {
        usdc = cfg.max_order_usdc;
        shares = usdc / trade.price;
    }
    if usdc < cfg.min_order_usdc {
        return Err(Skip::BelowMin { usdc });
    }

    // Marketable-limit price: cross the book by an absolute offset (in price
    // units), e.g. target 0.50 + 0.02 -> BUY limit 0.52.
    let slip = cfg.max_slippage;
    let limit_price = match trade.side {
        Side::Buy => (trade.price + slip).min(0.99),
        Side::Sell => (trade.price - slip).max(0.01),
    };
    // Clamp to the valid 2-decimal tick range. The old 0.999 cap rounded to
    // cents became 1.00, which the CLOB rejects ("too large for tick size") —
    // that silently killed every copy once slippage pushed the limit to the cap.
    let limit_price = round_price(limit_price).clamp(0.01, 0.99);
    let shares = round_shares(shares);

    Ok(CopyOrder {
        token_id: trade.token_id.clone(),
        side: trade.side,
        price: limit_price,
        ref_price: trade.price,
        size_shares: shares,
        usdc: shares * limit_price,
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
