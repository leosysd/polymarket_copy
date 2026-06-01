//! Dry-run executor: never touches the network, just reports what it *would*
//! do. The ledger is written centrally (for both dry-run and live) in main.

use super::{ExecOutcome, OrderExecutor};
use crate::models::CopyOrder;
use anyhow::Result;
use async_trait::async_trait;

pub struct DryRunExecutor;

impl DryRunExecutor {
    pub fn new() -> DryRunExecutor {
        DryRunExecutor
    }
}

#[async_trait]
impl OrderExecutor for DryRunExecutor {
    async fn execute(&self, order: &CopyOrder) -> Result<ExecOutcome> {
        Ok(ExecOutcome {
            submitted: false,
            detail: format!(
                "DRY_RUN 模拟：本会 {} {:.2} 份 @ {:.3}（~{:.2} USDC）",
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
