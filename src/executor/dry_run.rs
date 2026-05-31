//! Dry-run executor: never touches the network. Logs each copy decision and
//! appends it to a JSONL ledger so you can audit what *would* have happened.

use super::{ExecOutcome, OrderExecutor};
use crate::models::CopyOrder;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct DryRunExecutor {
    ledger: PathBuf,
    lock: Mutex<()>,
}

#[derive(Serialize)]
struct LedgerRow<'a> {
    ts: String,
    mode: &'a str,
    side: &'a str,
    token_id: &'a str,
    price: f64,
    ref_price: f64,
    size_shares: f64,
    usdc: f64,
    target: &'a str,
    target_label: &'a str,
    title: Option<&'a str>,
    outcome: Option<&'a str>,
    source_key: &'a str,
}

impl DryRunExecutor {
    pub fn new(ledger: PathBuf) -> DryRunExecutor {
        DryRunExecutor {
            ledger,
            lock: Mutex::new(()),
        }
    }

    fn append(&self, order: &CopyOrder) -> Result<()> {
        let row = LedgerRow {
            ts: chrono::Utc::now().to_rfc3339(),
            mode: "dry_run",
            side: order.side.as_str(),
            token_id: &order.token_id,
            price: order.price,
            ref_price: order.ref_price,
            size_shares: order.size_shares,
            usdc: order.usdc,
            target: &order.target,
            target_label: &order.target_label,
            title: order.title.as_deref(),
            outcome: order.outcome.as_deref(),
            source_key: &order.source_key,
        };
        let line = serde_json::to_string(&row)?;

        let _guard = self.lock.lock().unwrap();
        if let Some(parent) = self.ledger.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.ledger)
            .with_context(|| format!("opening ledger {}", self.ledger.display()))?;
        writeln!(f, "{line}").context("writing ledger row")?;
        Ok(())
    }
}

#[async_trait]
impl OrderExecutor for DryRunExecutor {
    async fn execute(&self, order: &CopyOrder) -> Result<ExecOutcome> {
        self.append(order)?;
        Ok(ExecOutcome {
            submitted: false,
            detail: format!(
                "DRY_RUN would {} {:.2} @ {:.3} (~{:.2} USDC)",
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
