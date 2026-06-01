//! 内置交互式管理菜单（`pmcopy menu`）。
//!
//! 编辑 config.toml（用 toml_edit 保留注释）和 .env，控制 systemd 服务，
//! 查看状态/跟单账本，派生 API key，也能前台启动机器人。

use crate::clob::{create_or_derive_api_creds, OrderSigner};
use anyhow::{anyhow, Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
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
            "状态",
            "设置（模式 / 份数 / 滑点 / 订单类型 / 金额）",
            "连接与密钥（WS 节点 / 私钥 / 资金地址）",
            "目标钱包（添加 / 删除）",
            "服务（systemd 启动 / 停止 / 重启）",
            "账本（最近跟单）",
            "派生并保存 API key",
            "立即运行（前台）",
            "退出",
        ];
        let choice = Select::with_theme(&theme)
            .with_prompt("管理")
            .items(&items)
            .default(0)
            .interact()?;

        match choice {
            0 => status(config_path)?,
            1 => settings_menu(config_path, &theme)?,
            2 => env_menu(&theme)?,
            3 => targets_menu(config_path, &theme)?,
            4 => service_menu(&theme)?,
            5 => show_ledger(config_path)?,
            6 => derive_key(config_path, http).await?,
            7 => run_foreground(config_path)?,
            _ => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 状态 / 概览
// ---------------------------------------------------------------------------

fn summary_line(doc: &DocumentMut) -> String {
    let mode = str_at(doc, &["mode"]).unwrap_or_else(|| "?".into());
    let factor = doc.get("copy_factor").and_then(|v| v.as_float()).unwrap_or(0.0);
    let n = doc
        .get("targets")
        .and_then(|t| t.as_array_of_tables())
        .map(|a| a.len())
        .unwrap_or(0);
    format!("── pmcopy ── 模式={mode}  目标={n}  copy_factor={factor} ──")
}

fn status(config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    println!();
    for k in [
        "mode",
        "copy_factor",
        "max_slippage",
        "order_type",
        "min_order_usdc",
        "max_order_usdc",
        "only_buys",
    ] {
        let v = doc
            .get(k)
            .map(|i| i.to_string().trim().to_string())
            .unwrap_or_else(|| "(默认)".into());
        println!("  {k:16}= {v}");
    }
    let n = doc
        .get("targets")
        .and_then(|t| t.as_array_of_tables())
        .map(|a| a.len())
        .unwrap_or(0);
    println!("  {:16}= {n}", "目标数");
    println!("  {:16}= {}", "PM_WSS_RPC", env_set("PM_WSS_RPC"));
    println!("  {:16}= {}", "PM_PRIVATE_KEY", env_set("PM_PRIVATE_KEY"));

    let ledger = ledger_path(&doc);
    let lines = std::fs::read_to_string(&ledger).map(|s| s.lines().count()).unwrap_or(0);
    println!("  {:16}= {lines} 行 ({})", "账本", ledger.display());
    Ok(())
}

fn env_set(key: &str) -> &'static str {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => "已设置",
        _ => "未设置",
    }
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

fn settings_menu(config_path: &Path, theme: &ColorfulTheme) -> Result<()> {
    let items = [
        "mode 模式 (dry_run / live)",
        "copy_factor 份数倍率",
        "max_slippage 滑点(绝对偏移)",
        "order_type 订单类型",
        "min_order_usdc 单笔最小金额",
        "max_order_usdc 单笔最大金额",
        "only_buys 只跟买入",
        "返回",
    ];
    loop {
        let mut doc = load_doc(config_path)?;
        let choice = Select::with_theme(theme)
            .with_prompt("要修改哪一项")
            .items(&items)
            .default(0)
            .interact()?;
        match choice {
            0 => {
                let modes = ["dry_run", "live"];
                let i = Select::with_theme(theme)
                    .with_prompt("模式")
                    .items(&modes)
                    .default(0)
                    .interact()?;
                doc["mode"] = value(modes[i]);
            }
            1 => doc["copy_factor"] = value(prompt_f64(theme, "copy_factor 份数倍率（如 0.25 = 跟 25%）")?),
            2 => doc["max_slippage"] = value(prompt_f64(theme, "max_slippage 价格偏移（如 0.02 → 0.50 挂 0.52）")?),
            3 => {
                let types = ["FAK", "FOK", "GTC", "GTD"];
                let i = Select::with_theme(theme)
                    .with_prompt("订单类型")
                    .items(&types)
                    .default(0)
                    .interact()?;
                doc["order_type"] = value(types[i]);
            }
            4 => doc["min_order_usdc"] = value(prompt_f64(theme, "min_order_usdc 单笔最小 USDC")?),
            5 => doc["max_order_usdc"] = value(prompt_f64(theme, "max_order_usdc 单笔最大 USDC")?),
            6 => {
                let on = Confirm::with_theme(theme)
                    .with_prompt("only_buys（只跟买入/进场，忽略卖出）？")
                    .interact()?;
                doc["only_buys"] = value(on);
            }
            _ => return Ok(()),
        }
        save_doc(config_path, &doc)?;
        println!("  已保存。（需重启服务才生效）");
    }
}

fn prompt_f64(theme: &ColorfulTheme, prompt: &str) -> Result<f64> {
    let s: String = Input::with_theme(theme).with_prompt(prompt).interact_text()?;
    s.trim().parse().context("不是有效数字")
}

// ---------------------------------------------------------------------------
// 连接与密钥（.env）
// ---------------------------------------------------------------------------

fn env_menu(theme: &ColorfulTheme) -> Result<()> {
    loop {
        let sig = std::env::var("PM_SIGNATURE_TYPE").unwrap_or_else(|_| "0".into());
        let items = [
            format!("PM_WSS_RPC（Polygon wss 节点）        [{}]", env_set("PM_WSS_RPC")),
            format!("PM_PRIVATE_KEY（下单私钥）            [{}]", env_set("PM_PRIVATE_KEY")),
            format!("PM_FUNDER_ADDRESS（资金地址，可空）   [{}]", env_set("PM_FUNDER_ADDRESS")),
            format!("PM_SIGNATURE_TYPE（账户类型）         [{sig}]"),
            "返回".to_string(),
        ];
        let choice = Select::with_theme(theme)
            .with_prompt("连接与密钥 (.env)")
            .items(&items)
            .default(0)
            .interact()?;
        match choice {
            0 => {
                let v: String = Input::with_theme(theme)
                    .with_prompt("PM_WSS_RPC (wss://...)")
                    .interact_text()?;
                set_secret("PM_WSS_RPC", v.trim())?;
            }
            1 => {
                let v: String = Password::with_theme(theme)
                    .with_prompt("PM_PRIVATE_KEY (0x...，输入时不显示)")
                    .interact()?;
                set_secret("PM_PRIVATE_KEY", v.trim())?;
            }
            2 => {
                let v: String = Input::with_theme(theme)
                    .with_prompt("PM_FUNDER_ADDRESS (留空=用私钥地址)")
                    .allow_empty(true)
                    .interact_text()?;
                set_secret("PM_FUNDER_ADDRESS", v.trim())?;
            }
            3 => {
                let opts = ["0  普通钱包 (EOA)", "1  邮箱/Magic 代理", "2  浏览器钱包 (Safe)"];
                let i = Select::with_theme(theme)
                    .with_prompt("PM_SIGNATURE_TYPE")
                    .items(&opts)
                    .default(0)
                    .interact()?;
                set_secret("PM_SIGNATURE_TYPE", &i.to_string())?;
            }
            _ => return Ok(()),
        }
    }
}

/// 写入 .env，并同步到当前进程环境（让状态显示立即生效）。
fn set_secret(key: &str, val: &str) -> Result<()> {
    set_env_var(key, val)?;
    std::env::set_var(key, val);
    println!("  已写入 {ENV_PATH}");
    Ok(())
}

// ---------------------------------------------------------------------------
// 目标钱包
// ---------------------------------------------------------------------------

fn targets_menu(config_path: &Path, theme: &ColorfulTheme) -> Result<()> {
    loop {
        let mut doc = load_doc(config_path)?;
        let labels = target_labels(&doc);

        println!("\n  当前目标：");
        if labels.is_empty() {
            println!("    （无）");
        } else {
            for (i, l) in labels.iter().enumerate() {
                println!("    {}. {l}", i + 1);
            }
        }

        let actions = ["添加目标", "删除目标", "返回"];
        let choice = Select::with_theme(theme)
            .with_prompt("目标钱包")
            .items(&actions)
            .default(0)
            .interact()?;
        match choice {
            0 => {
                let address: String = Input::with_theme(theme)
                    .with_prompt("钱包地址 (0x...)")
                    .interact_text()?;
                let address = address.trim().to_string();
                if !(address.len() == 42 && address.starts_with("0x")) {
                    println!("  地址无效，已跳过。");
                    continue;
                }
                let weight = prompt_f64(theme, "权重 (1.0 = 完整 copy_factor)").unwrap_or(1.0);
                let label: String = Input::with_theme(theme)
                    .with_prompt("标签（备注名）")
                    .default(address.clone())
                    .interact_text()?;

                let mut t = Table::new();
                t["address"] = value(address);
                t["weight"] = value(weight);
                t["label"] = value(label);
                ensure_targets(&mut doc).push(t);
                save_doc(config_path, &doc)?;
                println!("  已添加。");
            }
            1 => {
                if labels.is_empty() {
                    continue;
                }
                let mut opts = labels.clone();
                opts.push("取消".into());
                let i = Select::with_theme(theme)
                    .with_prompt("删除哪个？")
                    .items(&opts)
                    .default(opts.len() - 1)
                    .interact()?;
                if i < labels.len() {
                    ensure_targets(&mut doc).remove(i);
                    save_doc(config_path, &doc)?;
                    println!("  已删除。");
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
// 服务控制
// ---------------------------------------------------------------------------

fn service_menu(theme: &ColorfulTheme) -> Result<()> {
    let actions = ["状态", "启动", "停止", "重启", "日志（跟随）", "返回"];
    let choice = Select::with_theme(theme)
        .with_prompt(format!("systemd 服务 '{SERVICE}'"))
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
        Err(e) => println!("  运行 {bin} 失败：{e}"),
    }
}

// ---------------------------------------------------------------------------
// 账本
// ---------------------------------------------------------------------------

fn show_ledger(config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    let path = ledger_path(&doc);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            println!("  暂无账本：{}", path.display());
            return Ok(());
        }
    };
    let lines: Vec<&str> = content.lines().collect();
    println!("\n  最近 {} 笔跟单（{}）：", 15.min(lines.len()), path.display());
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
// 派生 API key -> .env
// ---------------------------------------------------------------------------

async fn derive_key(config_path: &Path, http: &Client) -> Result<()> {
    let pk = std::env::var("PM_PRIVATE_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("请先在 .env 设置 PM_PRIVATE_KEY"))?;

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
    println!("  正在用 {} 派生 ...", signer.address().to_checksum(None));
    let creds = create_or_derive_api_creds(http, &clob, &signer, 0).await?;

    set_env_var("PM_API_KEY", &creds.api_key)?;
    set_env_var("PM_API_SECRET", &creds.secret)?;
    set_env_var("PM_API_PASSPHRASE", &creds.passphrase)?;
    println!("  API 凭证已派生并写入 {ENV_PATH}");
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
    std::fs::write(ENV_PATH, out.join("\n") + "\n").context("写入 .env")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 前台运行
// ---------------------------------------------------------------------------

fn run_foreground(config_path: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("定位 pmcopy 程序")?;
    println!("  启动机器人（Ctrl-C 停止并返回菜单）...\n");
    let _ = Command::new(exe)
        .arg("--config")
        .arg(config_path)
        .status();
    Ok(())
}

// ---------------------------------------------------------------------------
// 配置文件辅助
// ---------------------------------------------------------------------------

fn ensure_config(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let example = Path::new("config.example.toml");
    if example.exists() {
        std::fs::copy(example, path)
            .with_context(|| format!("从 example 复制到 {}", path.display()))?;
        println!("已从 config.example.toml 创建 {}", path.display());
        Ok(())
    } else {
        Err(anyhow!("找不到 {}，也没有 config.example.toml", path.display()))
    }
}

fn load_doc(path: &Path) -> Result<DocumentMut> {
    std::fs::read_to_string(path)
        .with_context(|| format!("读取 {}", path.display()))?
        .parse::<DocumentMut>()
        .with_context(|| format!("解析 {}", path.display()))
}

fn save_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    std::fs::write(path, doc.to_string()).with_context(|| format!("写入 {}", path.display()))
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
