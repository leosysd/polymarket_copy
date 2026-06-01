//! Polymarket copy-trading bot (low-latency, on-chain).
//!
//! Subscribes to Polygon `OrderFilled` events for one or more target wallets and
//! mirrors each new fill onto your own account, scaled proportionally to the
//! target's size. Runs in DRY_RUN by default (logs decisions, places no orders).

mod clob;
mod config;
mod executor;
mod menu;
mod models;
mod monitor;
mod sizing;
mod state;

use crate::clob::{create_or_derive_api_creds, OrderSigner};
use crate::config::{Config, Mode, Target};
use crate::executor::{ClobExecutor, DryRunExecutor, ExecOutcome, OrderExecutor};
use crate::models::{CopyOrder, TargetTrade};
use crate::monitor::ChainMonitor;
use crate::state::State;
use alloy::primitives::Address;
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use reqwest::Client;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::signal;
use tracing::{debug, info, warn};

#[derive(Parser, Debug)]
#[command(name = "pmcopy", about = "Polymarket copy-trading bot (on-chain, low-latency)")]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long, global = true, default_value = "config.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Interactive management menu (configure, control service, view ledger).
    Menu,
    /// Derive CLOB API credentials from PM_PRIVATE_KEY and print them for .env.
    DeriveKey,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    init_tracing();

    let http = Client::builder()
        .user_agent(concat!("pmcopy/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?;

    // The menu must work before a full (valid) config exists, so handle it first.
    if let Some(Command::Menu) = args.command {
        return menu::run(&args.config, &http).await;
    }

    let mut cfg = Config::load(&args.config)?;
    if let Some(Command::DeriveKey) = args.command {
        return derive_and_print(&http, &cfg).await;
    }

    // Index targets by lowercased address for quick lookup from on-chain events.
    let by_addr: HashMap<String, Target> = cfg
        .file
        .targets
        .iter()
        .map(|t| (t.address.to_lowercase(), t.clone()))
        .collect();

    let executor: Box<dyn OrderExecutor> = match cfg.file.mode {
        Mode::DryRun => Box::new(DryRunExecutor::new()),
        Mode::Live => {
            if cfg.needs_api_creds() {
                info!("CLOB API credentials not set — deriving from PM_PRIVATE_KEY");
                let creds = derive_creds(&http, &cfg).await?;
                cfg.secrets.api_key = Some(creds.api_key);
                cfg.secrets.api_secret = Some(creds.secret);
                cfg.secrets.api_passphrase = Some(creds.passphrase);
                info!("derived CLOB API key successfully");
            }
            Box::new(ClobExecutor::new(
                http.clone(),
                &cfg.file.endpoints,
                &cfg.secrets,
                cfg.file.order_type.clone(),
            )?)
        }
    };

    let mut state = State::load(Path::new(&cfg.file.state.state_file))?;

    // Build the on-chain monitor.
    let sources = parse_addresses(&cfg.file.endpoints.log_sources)
        .context("parsing log_sources addresses")?;
    let targets = parse_addresses(
        &cfg.file
            .targets
            .iter()
            .map(|t| t.address.clone())
            .collect::<Vec<_>>(),
    )
    .context("parsing target addresses")?;

    info!(
        mode = ?cfg.file.mode,
        executor = executor.label(),
        targets = targets.len(),
        copy_factor = cfg.file.copy_factor,
        "polymarket copy-trading bot starting"
    );
    if cfg.file.mode == Mode::Live {
        warn!("LIVE mode: real orders will be submitted with real funds");
    }

    // Window alignment: if we start mid-window, don't trade the current
    // 5-minute window — wait until the next boundary (epoch multiple of 300s).
    let now = chrono::Utc::now().timestamp();
    let trade_enabled_at = ((now + 299) / 300) * 300;
    if trade_enabled_at > now {
        info!(
            wait_s = trade_enabled_at - now,
            "启动于 5 分钟窗口中途：对齐到下个窗口边界后才开始下单"
        );
    }

    let monitor = ChainMonitor::new(cfg.wss_rpc().to_string(), sources, targets);
    let mut trades = monitor.spawn();

    loop {
        tokio::select! {
            maybe_trade = trades.recv() => {
                match maybe_trade {
                    Some(trade) => {
                        handle_trade(&trade, &by_addr, &cfg, &mut state, executor.as_ref(), trade_enabled_at).await;
                        if let Err(e) = state.save() {
                            warn!(error = %e, "failed to persist state");
                        }
                    }
                    None => {
                        warn!("monitor channel closed; exiting");
                        break;
                    }
                }
            }
            _ = signal::ctrl_c() => {
                info!("received ctrl-c, shutting down");
                break;
            }
        }
    }

    state.save()?;
    Ok(())
}

async fn handle_trade(
    trade: &TargetTrade,
    by_addr: &HashMap<String, Target>,
    cfg: &Config,
    state: &mut State,
    executor: &dyn OrderExecutor,
    trade_enabled_at: i64,
) {
    let key = trade.dedup_key();
    if state.has_seen(&key) {
        return;
    }

    let Some(target) = by_addr.get(&trade.target.to_lowercase()) else {
        // Shouldn't happen (the filter is by target), but be defensive.
        debug!(target = %trade.target, "fill from untracked address; ignoring");
        return;
    };

    info!(
        target = %target.label.clone().unwrap_or_else(|| target.address.clone()),
        side = trade.side.as_str(),
        shares = trade.shares,
        price = format!("{:.3}", trade.price),
        usdc = format!("{:.2}", trade.usdc),
        tx = %trade.tx_hash,
        "target fill"
    );

    // Window-alignment gate (applies to dry_run and live alike).
    let now = chrono::Utc::now().timestamp();
    if now < trade_enabled_at {
        info!(wait_s = trade_enabled_at - now, "窗口对齐中，本笔不下单");
        state.mark_seen(key);
        return;
    }

    match sizing::build_order(trade, target, &cfg.file) {
        Ok(order) => match executor.execute(&order).await {
            Ok(out) => {
                // Our own latency: from receiving the on-chain fill to submitting.
                let proc_ms = trade.received_at.elapsed().as_millis();
                info!(
                    side = order.side.as_str(),
                    shares = order.size_shares,
                    price = order.price,
                    usdc = format!("{:.2}", order.usdc),
                    submitted = out.submitted,
                    proc_ms,
                    detail = %out.detail,
                    "COPY"
                );
                append_ledger(cfg, &order, &out, proc_ms);
            }
            Err(e) => warn!(error = %e, key = %key, "order execution failed"),
        },
        Err(skip) => debug!(reason = %skip, "skip trade"),
    }

    // Mark seen after the attempt so a restart never double-submits.
    state.mark_seen(key);
}

/// Append one row to the JSONL ledger for every copy — dry-run or live alike,
/// so there's always a record of what the bot did (and whether it submitted).
fn append_ledger(cfg: &Config, order: &CopyOrder, out: &ExecOutcome, proc_ms: u128) {
    let row = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "mode": match cfg.file.mode { Mode::DryRun => "dry_run", Mode::Live => "live" },
        "submitted": out.submitted,
        "side": order.side.as_str(),
        "size_shares": order.size_shares,
        "price": order.price,
        "ref_price": order.ref_price,
        "usdc": order.usdc,
        "proc_ms": proc_ms,
        "token_id": order.token_id,
        "target_label": order.target_label,
        "detail": out.detail,
    });
    let path = Path::new(&cfg.file.state.ledger_file);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    use std::io::Write;
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            let _ = writeln!(f, "{row}");
        }
        Err(e) => warn!(error = %e, "failed to write ledger"),
    }
}

fn parse_addresses(raw: &[String]) -> Result<Vec<Address>> {
    raw.iter()
        .map(|s| s.parse::<Address>().with_context(|| format!("invalid address {s}")))
        .collect()
}

/// Derive CLOB API credentials from the configured private key.
async fn derive_creds(http: &Client, cfg: &Config) -> Result<crate::clob::DerivedCreds> {
    let pk = cfg
        .secrets
        .private_key
        .as_ref()
        .ok_or_else(|| anyhow!("PM_PRIVATE_KEY is required to derive API credentials"))?;
    let signer = OrderSigner::new(pk, cfg.file.endpoints.chain_id, &cfg.file.endpoints.exchange)?;
    create_or_derive_api_creds(http, &cfg.file.endpoints.clob, &signer, 0).await
}

/// `derive-key` subcommand: print credentials ready to paste into `.env`.
async fn derive_and_print(http: &Client, cfg: &Config) -> Result<()> {
    let creds = derive_creds(http, cfg).await?;
    println!("# CLOB API credentials derived from PM_PRIVATE_KEY — paste into .env:");
    println!("PM_API_KEY={}", creds.api_key);
    println!("PM_API_SECRET={}", creds.secret);
    println!("PM_API_PASSPHRASE={}", creds.passphrase);
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(false).init();
}
