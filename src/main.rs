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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
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

    let cfg = Config::load(&args.config)?;
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
        Mode::DryRun => Box::new(DryRunExecutor::new(cfg.file.order_style)),
        Mode::Live => {
            info!("authenticating with Polymarket CLOB v2 (official SDK)...");
            let exec =
                ClobExecutor::new(&cfg.secrets, cfg.file.order_style, &cfg.file.order_type).await?;
            info!("CLOB v2 authenticated");
            Box::new(exec)
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

    // Window alignment (opt-in): if enabled and we start mid-window, wait until
    // the next 5-minute boundary before trading. Default off so a restart takes
    // effect immediately instead of losing a window.
    let now = chrono::Utc::now().timestamp();
    let trade_enabled_at = if cfg.file.align_to_window {
        ((now + 299) / 300) * 300
    } else {
        0
    };
    if trade_enabled_at > now {
        info!(
            wait_s = trade_enabled_at - now,
            "启动于 5 分钟窗口中途：对齐到下个窗口边界后才开始下单"
        );
    } else {
        info!("立即开始跟单（未启用窗口对齐）");
    }

    let monitor = ChainMonitor::new(cfg.wss_rpc().to_string(), sources, targets);
    let mut trades = monitor.spawn();

    // token_id -> (market slug, outcome), resolved once via Gamma and cached.
    let mut slug_cache: HashMap<String, (String, String)> = HashMap::new();
    // token_id -> cumulative USDC we've placed, for the per-market exposure cap.
    let mut spent: HashMap<String, f64> = HashMap::new();
    // Fills awaiting aggregation, keyed by target|token|side. Flushed once the
    // oldest fill in the group is `aggregate_window_ms` old.
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let agg_window = Duration::from_millis(cfg.file.aggregate_window_ms);

    // A small flush cadence drives both the aggregation deadline check and the
    // (debounced) state persist, so neither blocks the receive path per-fill.
    let mut flush_tick = tokio::time::interval(Duration::from_millis(50));
    flush_tick.tick().await; // drop the immediate first tick
    let mut save_tick = tokio::time::interval(Duration::from_secs(2));
    save_tick.tick().await;

    loop {
        tokio::select! {
            maybe_trade = trades.recv() => {
                match maybe_trade {
                    Some(trade) => {
                        let key = trade.dedup_key();
                        if state.has_seen(&key) {
                            continue;
                        }
                        if cfg.file.aggregate_window_ms == 0 {
                            // No aggregation: copy each fill on its own.
                            copy_one(&trade, 1, &by_addr, &cfg, executor.as_ref(),
                                trade_enabled_at, &http, &mut slug_cache, &mut spent).await;
                            state.mark_seen(key);
                        } else {
                            let pkey = format!(
                                "{}|{}|{}",
                                trade.target.to_lowercase(), trade.token_id, trade.side.as_str()
                            );
                            pending.entry(pkey)
                                .or_insert_with(|| Pending::new(&trade))
                                .add(&trade, key);
                        }
                    }
                    None => {
                        warn!("monitor channel closed; exiting");
                        break;
                    }
                }
            }
            _ = flush_tick.tick() => {
                let ready: Vec<String> = pending
                    .iter()
                    .filter(|(_, p)| p.first_at.elapsed() >= agg_window)
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in ready {
                    if let Some(p) = pending.remove(&k) {
                        let (trade, keys) = p.into_trade();
                        copy_one(&trade, keys.len(), &by_addr, &cfg, executor.as_ref(),
                            trade_enabled_at, &http, &mut slug_cache, &mut spent).await;
                        for key in keys {
                            state.mark_seen(key);
                        }
                    }
                }
            }
            _ = save_tick.tick() => {
                if let Err(e) = state.save() {
                    warn!(error = %e, "failed to persist state");
                }
            }
            _ = signal::ctrl_c() => {
                info!("received ctrl-c, shutting down");
                break;
            }
        }
    }

    // Drain anything still aggregating so a clean shutdown never drops a fill.
    for (_, p) in pending.drain() {
        let (trade, keys) = p.into_trade();
        copy_one(&trade, keys.len(), &by_addr, &cfg, executor.as_ref(),
            trade_enabled_at, &http, &mut slug_cache, &mut spent).await;
        for key in keys {
            state.mark_seen(key);
        }
    }

    state.save()?;
    Ok(())
}

/// Fills of the same (target, outcome, side) accumulated within the aggregation
/// window, to be mirrored as a single combined order.
struct Pending {
    target: String,
    side: crate::models::Side,
    token_id: String,
    shares: f64,
    usdc: f64,
    /// Earliest fill instant in the group — drives the flush deadline and proc_ms.
    first_at: Instant,
    /// Wall-clock + block time of the earliest fill (for detection latency).
    recv_unix_ms: i64,
    block_time: Option<u64>,
    tx_hash: String,
    /// Dedup keys to mark seen once the group is flushed.
    keys: Vec<String>,
    /// In-window dedup (subscription + getLogs can deliver the same fill twice).
    seen: HashSet<String>,
}

impl Pending {
    fn new(t: &TargetTrade) -> Pending {
        Pending {
            target: t.target.clone(),
            side: t.side,
            token_id: t.token_id.clone(),
            shares: 0.0,
            usdc: 0.0,
            first_at: t.received_at,
            recv_unix_ms: t.recv_unix_ms,
            block_time: t.block_time,
            tx_hash: t.tx_hash.clone(),
            keys: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn add(&mut self, t: &TargetTrade, key: String) {
        if !self.seen.insert(key.clone()) {
            return; // duplicate delivery within the window
        }
        self.shares += t.shares;
        self.usdc += t.usdc;
        if t.received_at < self.first_at {
            self.first_at = t.received_at;
            self.recv_unix_ms = t.recv_unix_ms;
            self.block_time = t.block_time;
        }
        self.keys.push(key);
    }

    /// Collapse into one synthetic trade priced at the size-weighted average.
    fn into_trade(self) -> (TargetTrade, Vec<String>) {
        let price = if self.shares > 0.0 { self.usdc / self.shares } else { 0.0 };
        (
            TargetTrade {
                target: self.target,
                side: self.side,
                token_id: self.token_id,
                price,
                shares: self.shares,
                usdc: self.usdc,
                tx_hash: self.tx_hash,
                log_index: 0,
                received_at: self.first_at,
                recv_unix_ms: self.recv_unix_ms,
                block_time: self.block_time,
            },
            self.keys,
        )
    }
}

/// Mirror one (possibly aggregated) target fill. Dedup/`mark_seen` is handled by
/// the caller; this only decides and submits. `n_fills` is how many on-chain
/// fills were coalesced into this order (1 when aggregation is off).
#[allow(clippy::too_many_arguments)]
async fn copy_one(
    trade: &TargetTrade,
    n_fills: usize,
    by_addr: &HashMap<String, Target>,
    cfg: &Config,
    executor: &dyn OrderExecutor,
    trade_enabled_at: i64,
    http: &Client,
    slug_cache: &mut HashMap<String, (String, String)>,
    spent: &mut HashMap<String, f64>,
) {
    let Some(target) = by_addr.get(&trade.target.to_lowercase()) else {
        // Shouldn't happen (the filter is by target), but be defensive.
        debug!(target = %trade.target, "fill from untracked address; ignoring");
        return;
    };

    // Market filter: only copy fills whose market slug matches (e.g. btc-updown-5m).
    let filter = cfg.file.market_filter.trim().to_lowercase();
    if !filter.is_empty() {
        let (slug, _) =
            cached_market(http, &cfg.file.endpoints.gamma, &trade.token_id, slug_cache).await;
        if !slug.to_lowercase().contains(&filter) {
            debug!(target = %trade.target, "非目标市场，跳过");
            return;
        }
    }

    info!(
        target = %target.label.clone().unwrap_or_else(|| target.address.clone()),
        side = trade.side.as_str(),
        shares = trade.shares,
        price = format!("{:.3}", trade.price),
        usdc = format!("{:.2}", trade.usdc),
        fills = n_fills,
        tx = %trade.tx_hash,
        "target fill"
    );

    // Window-alignment gate (applies to dry_run and live alike).
    let now = chrono::Utc::now().timestamp();
    if now < trade_enabled_at {
        info!(wait_s = trade_enabled_at - now, "窗口对齐中，本笔不下单");
        return;
    }

    match sizing::build_order(trade, target, &cfg.file) {
        Ok(order) => {
            // Per-market exposure cap: stop adding to a token once we've placed
            // `max_market_usdc` on it (counts dry-run too, so the ceiling can be
            // validated before going live).
            let cap = cfg.file.max_market_usdc;
            if cap > 0.0 {
                let already = spent.get(&order.token_id).copied().unwrap_or(0.0);
                if already + order.usdc > cap {
                    info!(
                        token = %order.token_id,
                        already = format!("{already:.2}"),
                        add = format!("{:.2}", order.usdc),
                        cap = format!("{cap:.2}"),
                        "达到单盘口累计上限，跳过"
                    );
                    return;
                }
            }
            match executor.execute(&order).await {
                Ok(out) => {
                    let used_usdc = out.accounted_usdc;
                    if used_usdc > 0.0 {
                        *spent.entry(order.token_id.clone()).or_insert(0.0) += used_usdc;
                    }
                    // proc_ms: our processing — from receiving the fill to submit.
                    let proc_ms = trade.received_at.elapsed().as_millis();
                    // detect_ms: chain → we received it. Needs the provider to put
                    // block_timestamp on the log (some omit it on subscriptions).
                    let detect_ms: Option<i64> = trade.block_time.and_then(|bt| {
                        let bt_ms = (bt as i64) * 1000;
                        (bt_ms > 0).then(|| (trade.recv_unix_ms - bt_ms).max(0))
                    });
                    // total_ms: full copy latency (target's fill mined → our submit).
                    let total_ms = detect_ms.map(|d| d + proc_ms as i64);
                    info!(
                        side = order.side.as_str(),
                        shares = order.size_shares,
                        price = order.price,
                        usdc = format!("{:.2}", order.usdc),
                        filled_shares = format!("{:.2}", out.filled_shares),
                        filled_usdc = format!("{:.2}", out.filled_usdc),
                        submitted = out.submitted,
                        proc_ms,
                        detect_ms = detect_ms.map(|d| d.to_string()).unwrap_or_else(|| "n/a".into()),
                        total_ms = total_ms.map(|d| d.to_string()).unwrap_or_else(|| "n/a".into()),
                        detail = %out.detail,
                        "COPY"
                    );
                    // Resolve the market (it's live now) and store slug+outcome in
                    // the ledger so the menu can group copies per market.
                    let (market, outcome) =
                        cached_market(http, &cfg.file.endpoints.gamma, &order.token_id, slug_cache).await;
                    append_ledger(cfg, &order, &out, proc_ms, detect_ms, &market, &outcome);
                }
                Err(e) => warn!(error = %e, token = %order.token_id, "order execution failed"),
            }
        }
        Err(skip) => debug!(reason = %skip, "skip trade"),
    }
}

/// Resolve a CLOB token id to (market slug, outcome) via the Gamma API.
async fn resolve_market(http: &Client, gamma: &str, token_id: &str) -> Option<(String, String)> {
    let url = format!("{}/markets?clob_token_ids={}", gamma.trim_end_matches('/'), token_id);
    // Short timeout: this can sit on the pre-trade path (market filter), so never
    // let a slow Gamma stall a copy for the full client timeout.
    let resp = http
        .get(&url)
        .timeout(Duration::from_millis(1500))
        .send()
        .await
        .ok()?;
    let text = resp.text().await.ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let m = v.get(0)?;
    let slug = m.get("slug")?.as_str()?.to_string();
    // outcomes/clobTokenIds are JSON-encoded string arrays, parallel by index.
    let outcome = (|| {
        let ids: Vec<String> = serde_json::from_str(m.get("clobTokenIds")?.as_str()?).ok()?;
        let outs: Vec<String> = serde_json::from_str(m.get("outcomes")?.as_str()?).ok()?;
        outs.get(ids.iter().position(|i| i == token_id)?).cloned()
    })()
    .unwrap_or_default();
    Some((slug, outcome))
}

/// Cached token_id -> (slug, outcome). One gamma call per token.
async fn cached_market(
    http: &Client,
    gamma: &str,
    token_id: &str,
    cache: &mut HashMap<String, (String, String)>,
) -> (String, String) {
    if let Some(v) = cache.get(token_id) {
        return v.clone();
    }
    let v = resolve_market(http, gamma, token_id).await.unwrap_or_default();
    cache.insert(token_id.to_string(), v.clone());
    v
}

/// Append one row to the JSONL ledger for every copy — dry-run or live alike,
/// so there's always a record of what the bot did (and whether it submitted).
fn append_ledger(
    cfg: &Config,
    order: &CopyOrder,
    out: &ExecOutcome,
    proc_ms: u128,
    detect_ms: Option<i64>,
    market: &str,
    outcome: &str,
) {
    let row = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "mode": match cfg.file.mode { Mode::DryRun => "dry_run", Mode::Live => "live" },
        "order_style": cfg.file.order_style.as_str(),
        "submitted": out.submitted,
        "market": market,
        "outcome": outcome,
        "side": order.side.as_str(),
        "size_shares": order.size_shares,
        "filled_shares": out.filled_shares,
        "price": order.price,
        "ref_price": order.ref_price,
        "usdc": order.usdc,
        "filled_usdc": out.filled_usdc,
        "accounted_usdc": out.accounted_usdc,
        "proc_ms": proc_ms,
        "detect_ms": detect_ms,
        "total_ms": detect_ms.map(|d| d + proc_ms as i64),
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
