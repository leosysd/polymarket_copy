//! Configuration: non-secret settings from `config.toml`, secrets from the
//! environment (`.env`). Secrets are only required in `live` mode.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    DryRun,
    Live,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    pub address: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub label: Option<String>,
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct Endpoints {
    pub clob: String,
    pub chain_id: u64,
    /// EIP-712 verifying contract for signing live orders.
    pub exchange: String,
    /// Gamma API base, used to resolve a token id to its market slug.
    #[serde(default = "default_gamma")]
    pub gamma: String,
    /// Contract addresses that emit the fill events we subscribe to. Defaults to
    /// the live Polymarket exchange verified on-chain.
    #[serde(default = "default_log_sources")]
    pub log_sources: Vec<String>,
}

fn default_log_sources() -> Vec<String> {
    vec![
        // Live Polymarket exchange settling BTC 5-minute (and other) markets.
        "0xe111180000d2663c0091e4f400237545b87b996b".to_string(),
        // Legacy CTF Exchange + NegRisk CTF Exchange (kept as fallbacks).
        "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E".to_string(),
        "0xC5d563A36AE78145C45a50134d48A1215220f80a".to_string(),
    ]
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatePaths {
    pub state_file: String,
    pub ledger_file: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileConfig {
    pub mode: Mode,
    pub copy_factor: f64,
    #[serde(default)]
    pub min_order_usdc: f64,
    #[serde(default = "default_max_usdc")]
    pub max_order_usdc: f64,
    #[serde(default)]
    pub only_buys: bool,
    /// If true, on startup wait until the next 5-minute boundary before trading
    /// (skip the in-progress window). Default false = trade immediately, so a
    /// restart takes effect at once instead of losing a window.
    #[serde(default)]
    pub align_to_window: bool,
    /// Absolute price offset to cross the book, in price units (e.g. 0.02 means
    /// a BUY at target 0.50 is limited at 0.52, a SELL at 0.48).
    #[serde(default = "default_slippage")]
    pub max_slippage: f64,
    /// Order time-in-force: "FAK" (fill what crosses now, cancel the rest —
    /// recommended for fast markets), "FOK" (all-or-nothing immediate), or
    /// "GTC" (leftover rests on the book).
    #[serde(default = "default_order_type")]
    pub order_type: String,
    /// Only copy fills whose market slug contains this (case-insensitive).
    /// Default limits copying to BTC 5-minute markets. Empty = all markets.
    #[serde(default = "default_market_filter")]
    pub market_filter: String,
    pub targets: Vec<Target>,
    pub endpoints: Endpoints,
    pub state: StatePaths,
}

fn default_max_usdc() -> f64 {
    50.0
}
fn default_slippage() -> f64 {
    0.02
}
fn default_order_type() -> String {
    "FAK".to_string()
}
fn default_market_filter() -> String {
    // Empty = no market filtering (copy all of the target's trades).
    String::new()
}
fn default_gamma() -> String {
    "https://gamma-api.polymarket.com".to_string()
}

/// Secrets pulled from the environment.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    /// Polygon WebSocket RPC URL (required — the monitor subscribes over it).
    pub wss_rpc: Option<String>,
    pub private_key: Option<String>,
    pub api_key: Option<String>,
    pub api_secret: Option<String>,
    pub api_passphrase: Option<String>,
    pub funder_address: Option<String>,
    pub signature_type: u8,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub file: FileConfig,
    pub secrets: Secrets,
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let mut file: FileConfig =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

        if file.copy_factor <= 0.0 {
            return Err(anyhow!("copy_factor must be > 0"));
        }
        file.order_type = file.order_type.to_uppercase();
        if !matches!(file.order_type.as_str(), "FAK" | "FOK" | "GTC" | "GTD") {
            return Err(anyhow!(
                "order_type must be one of FAK, FOK, GTC, GTD (got {})",
                file.order_type
            ));
        }
        if file.targets.is_empty() {
            return Err(anyhow!("no targets configured"));
        }

        let secrets = Secrets {
            wss_rpc: env_opt("PM_WSS_RPC"),
            private_key: env_opt("PM_PRIVATE_KEY"),
            api_key: env_opt("PM_API_KEY"),
            api_secret: env_opt("PM_API_SECRET"),
            api_passphrase: env_opt("PM_API_PASSPHRASE"),
            funder_address: env_opt("PM_FUNDER_ADDRESS"),
            signature_type: env_opt("PM_SIGNATURE_TYPE")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        };

        let cfg = Config { file, secrets };
        if cfg.secrets.wss_rpc.is_none() {
            return Err(anyhow!(
                "PM_WSS_RPC is required (a Polygon wss:// endpoint, e.g. from Alchemy)"
            ));
        }
        if cfg.file.mode == Mode::Live {
            cfg.validate_live()?;
        }
        Ok(cfg)
    }

    pub fn wss_rpc(&self) -> &str {
        self.secrets.wss_rpc.as_deref().unwrap_or_default()
    }

    fn validate_live(&self) -> Result<()> {
        // Only the private key is mandatory. If the CLOB API credentials are
        // absent they are derived from the key at startup.
        if self.secrets.private_key.is_none() {
            return Err(anyhow!("live mode requires PM_PRIVATE_KEY"));
        }
        Ok(())
    }

    /// True when any of the three CLOB API credentials is missing.
    pub fn needs_api_creds(&self) -> bool {
        self.secrets.api_key.is_none()
            || self.secrets.api_secret.is_none()
            || self.secrets.api_passphrase.is_none()
    }
}
