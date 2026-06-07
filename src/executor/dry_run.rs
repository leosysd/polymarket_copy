//! Dry-run executor: never touches the network, just reports what it would do.
//! The ledger is written centrally (for both dry-run and live) in main.

use super::{ExecOutcome, OrderExecutor};
use crate::config::OrderStyle;
use crate::models::CopyOrder;
use anyhow::Result;
use async_trait::async_trait;

pub struct DryRunExecutor {
    order_style: OrderStyle,
}

impl DryRunExecutor {
    pub fn new(order_style: OrderStyle) -> DryRunExecutor {
        DryRunExecutor { order_style }
    }
}

#[async_trait]
impl OrderExecutor for DryRunExecutor {
    async fn execute(&self, order: &CopyOrder) -> Result<ExecOutcome> {
        Ok(ExecOutcome {
            submitted: false,
            filled_shares: order.size_shares,
            filled_usdc: order.usdc,
            accounted_usdc: order.usdc,
            detail: format!(
                "DRY_RUN {}: would {} {:.2} shares at {:.3} (~{:.2} USDC)",
                self.order_style.as_str(),
                order.side.as_str(),
                order.size_shares,
                order.price,
                order.usdc
            ),
        })
    }

    fn label(&self) -> &'static str {
        "dry_run"
    }
}
