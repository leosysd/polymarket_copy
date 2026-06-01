//! Built-in interactive management menu (`pmcopy menu`).
//!
//! Edits `config.toml` (preserving comments via toml_edit) and `.env`, controls
//! the systemd service, shows status / the copy ledger, derives API keys, and
//! can launch the bot in the foreground.

use crate::clob::{create_or_derive_api_creds, OrderSigner};
use anyhow::{anyhow, Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{value, DocumentMut, Item, Table};

const ENV_PATH: &str = ".env";
const SERVICE: &str = "pmcopy";

pub async fn run(config_path: &Path, http: &Client) -> Result<()> {
    ensure_config(config_path)?;
    let theme = ColorfulTheme::default();

    loop {
        let doc = load_doc(config_path)?;
        println!("\n{}", summary_line(&doc));

        let items = [
            "Status",
            "Settings (mode, sizing, slippage, order type)",
            "Targets (add / remove wallets)",
            "Service (systemd start/stop/restart)",
            "Ledger (recent copies)",
            "Derive & save API key",
            "Run now (foreground)",
            "Quit",
        ];
        let choice = Select::with_theme(&theme)
            .with_prompt("Manage")
            .items(&items)
            .default(0)
            .interact()?;

        match choice {
            0 => status(config_path)?,
            1 => settings_menu(config_path, &theme)?,
            2 => targets_menu(config_path, &theme)?,
            3 => service_menu(&theme)?,
            4 => show_ledger(config_path)?,
            5 => derive_key(config_path, http).await?,
            6 => run_foreground(config_path)?,
            _ => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Status / summary
// ---------------------------------------------------------------------------

fn summary_line(doc: &DocumentMut) -> String {
    let mode = str_at(doc, &["mode"]).unwrap_or_else(|| "?".into());
    let factor = doc.get("copy_factor").and_then(|v| v.as_float()).unwrap_or(0.0);
    let n = doc
        .get("targets")
        .and_then(|t| t.as_array_of_tables())
        .map(|a| a.len())
        .unwrap_or(0);
    format!("── pmcopy ── mode={mode}  targets={n}  copy_factor={factor} ──")
}

fn status(config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    println!();
    for (k, path) in [
        ("mode", vec!["mode"]),
        ("copy_factor", vec!["copy_factor"]),
        ("max_slippage", vec!["max_slippage"]),
        ("order_type", vec!["order_type"]),
        ("min_order_usdc", vec!["min_order_usdc"]),
        ("max_order_usdc", vec!["max_order_usdc"]),
        ("only_buys", vec!["only_buys"]),
    ] {
        let v = doc
            .get(path[0])
            .map(|i| i.to_string().trim().to_string())
            .unwrap_or_else(|| "(default)".into());
        println!("  {k:16}= {v}");
    }
    let n = doc
        .get("targets")
        .and_then(|t| t.as_array_of_tables())
        .map(|a| a.len())
        .unwrap_or(0);
    println!("  {:16}= {n}", "targets");
    println!("  {:16}= {}", "PM_WSS_RPC", env_set("PM_WSS_RPC"));
    println!("  {:16}= {}", "PM_PRIVATE_KEY", env_set("PM_PRIVATE_KEY"));

    let ledger = ledger_path(&doc);
    let lines = std::fs::read_to_string(&ledger).map(|s| s.lines().count()).unwrap_or(0);
    println!("  {:16}= {lines} rows ({})", "copies ledger", ledger.display());
    Ok(())
}

fn env_set(key: &str) -> &'static str {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => "set",
        _ => "NOT set",
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn settings_menu(config_path: &Path, theme: &ColorfulTheme) -> Result<()> {
    let items = [
        "mode (dry_run / live)",
        "copy_factor",
        "max_slippage",
        "order_type",
        "min_order_usdc",
        "max_order_usdc",
        "only_buys",
        "Back",
    ];
    loop {
        let mut doc = load_doc(config_path)?;
        let choice = Select::with_theme(theme)
            .with_prompt("Setting to change")
            .items(&items)
            .default(0)
            .interact()?;
        match choice {
            0 => {
                let modes = ["dry_run", "live"];
                let i = Select::with_theme(theme)
                    .with_prompt("mode")
                    .items(&modes)
                    .default(0)
                    .interact()?;
                doc["mode"] = value(modes[i]);
            }
            1 => doc["copy_factor"] = value(prompt_f64(theme, "copy_factor (e.g. 0.25)")?),
            2 => doc["max_slippage"] = value(prompt_f64(theme, "max_slippage (e.g. 0.02)")?),
            3 => {
                let types = ["FAK", "FOK", "GTC", "GTD"];
                let i = Select::with_theme(theme)
                    .with_prompt("order_type")
                    .items(&types)
                    .default(0)
                    .interact()?;
                doc["order_type"] = value(types[i]);
            }
            4 => doc["min_order_usdc"] = value(prompt_f64(theme, "min_order_usdc")?),
            5 => doc["max_order_usdc"] = value(prompt_f64(theme, "max_order_usdc")?),
            6 => {
                let on = Confirm::with_theme(theme)
                    .with_prompt("only_buys (mirror entries only)?")
                    .interact()?;
                doc["only_buys"] = value(on);
            }
            _ => return Ok(()),
        }
        save_doc(config_path, &doc)?;
        println!("  saved. (restart the service for changes to take effect)");
    }
}

fn prompt_f64(theme: &ColorfulTheme, prompt: &str) -> Result<f64> {
    let s: String = Input::with_theme(theme).with_prompt(prompt).interact_text()?;
    s.trim().parse().context("not a number")
}

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

fn targets_menu(config_path: &Path, theme: &ColorfulTheme) -> Result<()> {
    loop {
        let mut doc = load_doc(config_path)?;
        let labels = target_labels(&doc);

        println!("\n  Current targets:");
        if labels.is_empty() {
            println!("    (none)");
        } else {
            for (i, l) in labels.iter().enumerate() {
                println!("    {}. {l}", i + 1);
            }
        }

        let actions = ["Add target", "Remove target", "Back"];
        let choice = Select::with_theme(theme)
            .with_prompt("Targets")
            .items(&actions)
            .default(0)
            .interact()?;
        match choice {
            0 => {
                let address: String = Input::with_theme(theme)
                    .with_prompt("wallet address (0x...)")
                    .interact_text()?;
                let address = address.trim().to_string();
                if !(address.len() == 42 && address.starts_with("0x")) {
                    println!("  invalid address; skipped.");
                    continue;
                }
                let weight = prompt_f64(theme, "weight (1.0 = full copy_factor)").unwrap_or(1.0);
                let label: String = Input::with_theme(theme)
                    .with_prompt("label")
                    .default(address.clone())
                    .interact_text()?;

                let mut t = Table::new();
                t["address"] = value(address);
                t["weight"] = value(weight);
                t["label"] = value(label);
                ensure_targets(&mut doc).push(t);
                save_doc(config_path, &doc)?;
                println!("  added.");
            }
            1 => {
                if labels.is_empty() {
                    continue;
                }
                let mut opts = labels.clone();
                opts.push("Cancel".into());
                let i = Select::with_theme(theme)
                    .with_prompt("Remove which?")
                    .items(&opts)
                    .default(opts.len() - 1)
                    .interact()?;
                if i < labels.len() {
                    ensure_targets(&mut doc).remove(i);
                    save_doc(config_path, &doc)?;
                    println!("  removed.");
                }
            }
            _ => return Ok(()),
        }
    }
}

fn target_labels(doc: &DocumentMut) -> Vec<String> {
    doc.get("targets")
        .and_then(|t| t.as_array_of_tables())
        .map(|arr| {
            arr.iter()
                .map(|t| {
                    let addr = t.get("address").and_then(|v| v.as_str()).unwrap_or("?");
                    match t.get("label").and_then(|v| v.as_str()) {
                        Some(l) => format!("{l}  ({addr})"),
                        None => addr.to_string(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ensure_targets(doc: &mut DocumentMut) -> &mut toml_edit::ArrayOfTables {
    if doc.get("targets").and_then(|t| t.as_array_of_tables()).is_none() {
        doc["targets"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    doc["targets"].as_array_of_tables_mut().unwrap()
}

// ---------------------------------------------------------------------------
// Service control
// ---------------------------------------------------------------------------

fn service_menu(theme: &ColorfulTheme) -> Result<()> {
    let actions = ["status", "start", "stop", "restart", "logs (follow)", "Back"];
    let choice = Select::with_theme(theme)
        .with_prompt(format!("systemd service '{SERVICE}'"))
        .items(&actions)
        .default(0)
        .interact()?;
    match choice {
        0 => run_cmd("systemctl", &["status", SERVICE, "--no-pager"]),
        1 => run_cmd("sudo", &["systemctl", "start", SERVICE]),
        2 => run_cmd("sudo", &["systemctl", "stop", SERVICE]),
        3 => run_cmd("sudo", &["systemctl", "restart", SERVICE]),
        4 => run_cmd("journalctl", &["-u", SERVICE, "-f", "--no-pager"]),
        _ => return Ok(()),
    }
    Ok(())
}

fn run_cmd(bin: &str, args: &[&str]) {
    match Command::new(bin).args(args).status() {
        Ok(_) => {}
        Err(e) => println!("  failed to run {bin}: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

fn show_ledger(config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    let path = ledger_path(&doc);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            println!("  no ledger yet at {}", path.display());
            return Ok(());
        }
    };
    let lines: Vec<&str> = content.lines().collect();
    println!("\n  last {} copies ({}):", 15.min(lines.len()), path.display());
    for line in lines.iter().rev().take(15).rev() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            println!(
                "    {}  {} {} @ {}  (~{} USDC)  {}",
                v["ts"].as_str().unwrap_or(""),
                v["side"].as_str().unwrap_or(""),
                v["size_shares"],
                v["price"],
                v["usdc"],
                v["target_label"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Derive API key -> .env
// ---------------------------------------------------------------------------

async fn derive_key(config_path: &Path, http: &Client) -> Result<()> {
    let pk = std::env::var("PM_PRIVATE_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("set PM_PRIVATE_KEY in .env first"))?;

    let doc = load_doc(config_path)?;
    let chain_id = doc
        .get("endpoints")
        .and_then(|e| e.get("chain_id"))
        .and_then(|v| v.as_integer())
        .unwrap_or(137) as u64;
    let exchange = str_at(&doc, &["endpoints", "exchange"])
        .unwrap_or_else(|| "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E".into());
    let clob = str_at(&doc, &["endpoints", "clob"])
        .unwrap_or_else(|| "https://clob.polymarket.com".into());

    let signer = OrderSigner::new(&pk, chain_id, &exchange)?;
    println!("  deriving from {} ...", signer.address().to_checksum(None));
    let creds = create_or_derive_api_creds(http, &clob, &signer, 0).await?;

    set_env_var("PM_API_KEY", &creds.api_key)?;
    set_env_var("PM_API_SECRET", &creds.secret)?;
    set_env_var("PM_API_PASSPHRASE", &creds.passphrase)?;
    println!("  API credentials derived and written to {ENV_PATH}");
    Ok(())
}

fn set_env_var(key: &str, val: &str) -> Result<()> {
    let content = std::fs::read_to_string(ENV_PATH).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in content.lines() {
        if line.trim_start().starts_with(&format!("{key}=")) {
            out.push(format!("{key}={val}"));
            replaced = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !replaced {
        out.push(format!("{key}={val}"));
    }
    std::fs::write(ENV_PATH, out.join("\n") + "\n").context("writing .env")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Run foreground
// ---------------------------------------------------------------------------

fn run_foreground(config_path: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("locating pmcopy binary")?;
    println!("  starting bot (Ctrl-C to stop and return to the menu)...\n");
    let _ = Command::new(exe)
        .arg("--config")
        .arg(config_path)
        .status();
    Ok(())
}

// ---------------------------------------------------------------------------
// Config doc helpers
// ---------------------------------------------------------------------------

fn ensure_config(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let example = Path::new("config.example.toml");
    if example.exists() {
        std::fs::copy(example, path)
            .with_context(|| format!("copying example config to {}", path.display()))?;
        println!("created {} from config.example.toml", path.display());
        Ok(())
    } else {
        Err(anyhow!("{} not found and no config.example.toml", path.display()))
    }
}

fn load_doc(path: &Path) -> Result<DocumentMut> {
    std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))
}

fn save_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
}

fn str_at(doc: &DocumentMut, path: &[&str]) -> Option<String> {
    let mut item: &Item = doc.get(path[0])?;
    for key in &path[1..] {
        item = item.as_table_like()?.get(key)?;
    }
    item.as_str().map(|s| s.to_string())
}

fn ledger_path(doc: &DocumentMut) -> PathBuf {
    str_at(doc, &["state", "ledger_file"])
        .unwrap_or_else(|| "data/copies.jsonl".into())
        .into()
}
