//! Polymarket copy-trading bot.
//!
//! Polls one or more target wallets' trade activity and mirrors each new trade
//! onto your own account, scaled proportionally to the target's size. Runs in
//! DRY_RUN by default (logs decisions, places no orders).

mod clob;
mod config;
mod executor;
mod models;
mod monitor;
mod sizing;
mod state;

use crate::config::{Config, Mode};
use crate::executor::{ClobExecutor, DryRunExecutor, OrderExecutor};
use crate::monitor::Monitor;
use crate::state::State;
use anyhow::Result;
use clap::Parser;
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::signal;
use tracing::{debug, info, warn};

#[derive(Parser, Debug)]
#[command(name = "pmcopy", about = "Polymarket copy-trading bot")]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Run a single poll cycle and exit (useful for testing).
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    init_tracing();

    let cfg = Config::load(&args.config)?;
    let http = Client::builder()
        .user_agent(concat!("pmcopy/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?;

    let monitor = Monitor::new(http.clone(), cfg.file.endpoints.data_api.clone());

    let executor: Box<dyn OrderExecutor> = match cfg.file.mode {
        Mode::DryRun => Box::new(DryRunExecutor::new(PathBuf::from(&cfg.file.state.ledger_file))),
        Mode::Live => Box::new(ClobExecutor::new(
            http.clone(),
            &cfg.file.endpoints,
            &cfg.secrets,
        )?),
    };

    let mut state = State::load(Path::new(&cfg.file.state.state_file))?;

    info!(
        mode = ?cfg.file.mode,
        executor = executor.label(),
        targets = cfg.file.targets.len(),
        copy_factor = cfg.file.copy_factor,
        poll_secs = cfg.file.poll_interval_secs,
        "polymarket copy-trading bot starting"
    );
    if cfg.file.mode == Mode::Live {
        warn!("LIVE mode: real orders will be submitted with real funds");
    }

    loop {
        if let Err(e) = poll_once(&monitor, &cfg, &mut state, executor.as_ref()).await {
            warn!(error = %e, "poll cycle failed");
        }
        if let Err(e) = state.save() {
            warn!(error = %e, "failed to persist state");
        }

        if args.once {
            break;
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(cfg.file.poll_interval_secs)) => {}
            _ = signal::ctrl_c() => {
                info!("received ctrl-c, shutting down");
                break;
            }
        }
    }

    state.save()?;
    Ok(())
}

async fn poll_once(
    monitor: &Monitor,
    cfg: &Config,
    state: &mut State,
    executor: &dyn OrderExecutor,
) -> Result<()> {
    let first_run = !state.bootstrapped;
    if first_run {
        info!("first run: recording current history without trading (bootstrap)");
    }

    for target in &cfg.file.targets {
        let items = match monitor.fetch_activity(&target.address).await {
            Ok(v) => v,
            Err(e) => {
                warn!(target = %target.address, error = %e, "failed to fetch activity");
                continue;
            }
        };

        // The API returns newest-first; process oldest-first for chronology.
        for item in items.iter().rev() {
            if !item.is_trade() {
                continue;
            }
            let key = item.dedup_key();
            if state.has_seen(&key) {
                continue;
            }

            if first_run {
                state.mark_seen(key);
                continue;
            }

            match sizing::build_order(item, target, &cfg.file) {
                Ok(order) => match executor.execute(&order).await {
                    Ok(out) => info!(
                        target = %order.target_label,
                        side = order.side.as_str(),
                        shares = order.size_shares,
                        price = order.price,
                        usdc = order.usdc,
                        title = order.title.as_deref().unwrap_or(""),
                        submitted = out.submitted,
                        detail = %out.detail,
                        "COPY"
                    ),
                    Err(e) => warn!(error = %e, key = %key, "order execution failed"),
                },
                Err(skip) => {
                    debug!(reason = %skip, target = %target.address, "skip trade");
                }
            }

            // Mark seen after the attempt so we never double-submit on restart.
            state.mark_seen(key);
        }
    }

    if first_run {
        state.set_bootstrapped();
        info!("bootstrap complete — only NEW trades will be copied from now on");
    }
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(false).init();
}
