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
    pub data_api: String,
    pub clob: String,
    pub chain_id: u64,
    pub exchange: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatePaths {
    pub state_file: String,
    pub ledger_file: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileConfig {
    pub mode: Mode,
    #[serde(default = "default_poll")]
    pub poll_interval_secs: u64,
    pub copy_factor: f64,
    #[serde(default)]
    pub min_order_usdc: f64,
    #[serde(default = "default_max_usdc")]
    pub max_order_usdc: f64,
    #[serde(default)]
    pub only_buys: bool,
    #[serde(default = "default_slippage")]
    pub max_slippage_bps: u32,
    pub targets: Vec<Target>,
    pub endpoints: Endpoints,
    pub state: StatePaths,
}

fn default_poll() -> u64 {
    8
}
fn default_max_usdc() -> f64 {
    50.0
}
fn default_slippage() -> u32 {
    150
}

/// Secrets pulled from the environment. Empty unless live trading.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
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
        let file: FileConfig =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

        if file.copy_factor <= 0.0 {
            return Err(anyhow!("copy_factor must be > 0"));
        }
        if file.targets.is_empty() {
            return Err(anyhow!("no targets configured"));
        }

        let secrets = Secrets {
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
        if cfg.file.mode == Mode::Live {
            cfg.validate_live()?;
        }
        Ok(cfg)
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
