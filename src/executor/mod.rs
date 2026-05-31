//! Order execution backends. The bot logic depends only on the `OrderExecutor`
//! trait; `dry_run` logs decisions, `clob` submits real signed orders.

mod clob;
mod dry_run;

pub use clob::ClobExecutor;
pub use dry_run::DryRunExecutor;

use crate::models::CopyOrder;
use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug)]
pub struct ExecOutcome {
    pub submitted: bool,
    pub detail: String,
}

#[async_trait]
pub trait OrderExecutor: Send + Sync {
    async fn execute(&self, order: &CopyOrder) -> Result<ExecOutcome>;
    fn label(&self) -> &'static str;
}
