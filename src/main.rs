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

use crate::clob::{create_or_derive_api_creds, OrderSigner};
use crate::config::{Config, Mode};
use crate::executor::{ClobExecutor, DryRunExecutor, OrderExecutor};
use crate::monitor::Monitor;
use crate::state::State;
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Derive CLOB API credentials from PM_PRIVATE_KEY and print them for .env.
    DeriveKey,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    init_tracing();

    let mut cfg = Config::load(&args.config)?;
    let http = Client::builder()
        .user_agent(concat!("pmcopy/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?;

    // Subcommand: derive credentials and print them, then exit.
    if let Some(Command::DeriveKey) = args.command {
        return derive_and_print(&http, &cfg).await;
    }

    let monitor = Monitor::new(http.clone(), cfg.file.endpoints.data_api.clone());

    let executor: Box<dyn OrderExecutor> = match cfg.file.mode {
        Mode::DryRun => Box::new(DryRunExecutor::new(PathBuf::from(&cfg.file.state.ledger_file))),
        Mode::Live => {
            // Auto-derive CLOB API credentials from the private key if absent.
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
            )?)
        }
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
