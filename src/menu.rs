//! 内置交互式管理菜单（`pmcopy menu`）。
//!
//! 编辑 config.toml（用 toml_edit 保留注释）和 .env，控制 systemd 服务，
//! 查看状态/跟单账本，派生 API key，也能前台启动机器人。

use crate::clob::{create_or_derive_api_creds, OrderSigner};
use anyhow::{anyhow, Context, Result};
use console::style;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
use reqwest::Client;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use toml_edit::{value, DocumentMut, Item, Table};

const ENV_PATH: &str = ".env";
const SERVICE: &str = "pmcopy";
const UNIT_PATH: &str = "/etc/systemd/system/pmcopy.service";

pub async fn run(config_path: &Path, http: &Client) -> Result<()> {
    ensure_config(config_path)?;
    let theme = ColorfulTheme::default();

    loop {
        let doc = load_doc(config_path)?;
        print_banner(&doc);

        let items = [
            "📊  状态",
            "⚙️   设置（跟单比例 / 滑点 / 模式）",
            "🔌  连接（节点 / 私钥）",
            "🎯  跟单地址（添加 / 删除）",
            "🚀  服务（安装 / 启停 / 重启）",
            "📜  实时日志（跟随，看有没有下单）",
            "📒  账本（最近跟单汇总）",
            "🔑  派生 API key",
            "▶   立即运行（前台）",
            "⬆️   更新程序",
            "❌  退出",
        ];
        let choice = Select::with_theme(&theme)
            .with_prompt("选择操作")
            .items(&items)
            .default(0)
            .interact()?;

        match choice {
            0 => status(config_path)?,
            1 => settings_menu(config_path, &theme)?,
            2 => env_menu(&theme)?,
            3 => targets_menu(config_path, &theme)?,
            4 => service_menu(config_path, &theme)?,
            5 => follow_log(config_path)?,
            6 => show_ledger(config_path)?,
            7 => derive_key(config_path, http).await?,
            8 => run_foreground(config_path)?,
            9 => update_self(&theme)?,
            _ => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 状态 / 概览
// ---------------------------------------------------------------------------

fn print_banner(doc: &DocumentMut) {
    let mode = str_at(doc, &["mode"]).unwrap_or_else(|| "?".into());
    let factor = doc.get("copy_factor").and_then(|v| v.as_float()).unwrap_or(0.0);
    let n = doc
        .get("targets")
        .and_then(|t| t.as_array_of_tables())
        .map(|a| a.len())
        .unwrap_or(0);

    let svc = if !service_installed() {
        style("未安装").yellow().bold()
    } else if service_running() {
        style("运行中").green().bold()
    } else {
        style("已停止").red().bold()
    };
    let mode_disp = if mode == "live" {
        style("实盘 ⚠").red().bold()
    } else {
        style("模拟").green().bold()
    };
    let wss = if env_set("PM_WSS_RPC") == "已设置" {
        style("✔ 已设置").green()
    } else {
        style("✘ 未设置").red()
    };

    println!("\n{}", style("══════════════════════════════════").cyan());
    println!("🤖  {}", style("pmcopy 跟单机器人 管理菜单").bold().cyan());
    println!("{}", style("══════════════════════════════════").cyan());
    println!("  服务: {svc}    模式: {mode_disp}");
    println!("  目标: {n} 个    copy_factor: {factor}    节点: {wss}");
}

fn service_installed() -> bool {
    Path::new(UNIT_PATH).exists()
}

fn service_running() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", SERVICE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
    loop {
        let mut doc = load_doc(config_path)?;
        let factor = doc.get("copy_factor").and_then(|v| v.as_float()).unwrap_or(0.0);
        let slip = doc.get("max_slippage").and_then(|v| v.as_float()).unwrap_or(0.0);
        let mode = str_at(&doc, &["mode"]).unwrap_or_default();
        let items = [
            format!("跟单比例 copy_factor      [{factor}]"),
            format!("滑点 max_slippage         [{slip}]"),
            format!("模式 mode                 [{mode}]"),
            "返回".to_string(),
        ];
        let choice = Select::with_theme(theme)
            .with_prompt("要修改哪一项")
            .items(&items)
            .default(0)
            .interact()?;
        match choice {
            0 => {
                doc["copy_factor"] =
                    value(prompt_f64(theme, "跟单比例（如 0.25 = 跟目标的 25%）")?)
            }
            1 => {
                doc["max_slippage"] =
                    value(prompt_f64(theme, "滑点价格偏移（如 0.02 → 目标 0.50 挂 0.52）")?)
            }
            2 => {
                let modes = ["dry_run  模拟（不真实下单）", "live  实盘 ⚠（真实资金）"];
                let i = Select::with_theme(theme)
                    .with_prompt("模式")
                    .items(&modes)
                    .default(0)
                    .interact()?;
                if i == 1
                    && !Confirm::with_theme(theme)
                        .with_prompt("切到实盘会用真实资金下单，确认？")
                        .default(false)
                        .interact()?
                {
                    continue;
                }
                doc["mode"] = value(if i == 0 { "dry_run" } else { "live" });
            }
            _ => return Ok(()),
        }
        save_doc(config_path, &doc)?;
        println!("  {} 已保存。", style("✔").green());
        offer_restart(theme)?;
    }
}

/// 改完配置后，若服务在跑就问要不要重启使之生效。
fn offer_restart(theme: &ColorfulTheme) -> Result<()> {
    if service_installed() && service_running() {
        if Confirm::with_theme(theme)
            .with_prompt("重启服务使改动生效？")
            .default(true)
            .interact()?
        {
            run_cmd("sudo", &["systemctl", "restart", SERVICE]);
        }
    } else {
        println!("  （改动将在下次启动机器人时生效）");
    }
    Ok(())
}

fn prompt_f64(theme: &ColorfulTheme, prompt: &str) -> Result<f64> {
    let s: String = Input::with_theme(theme).with_prompt(prompt).interact_text()?;
    s.trim().parse().context("不是有效数字")
}

// ---------------------------------------------------------------------------
// 连接与密钥（.env）
// ---------------------------------------------------------------------------

fn env_menu(theme: &ColorfulTheme) -> Result<()> {
    let mut changed = false;
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
            _ => {
                if changed {
                    offer_restart(theme)?;
                }
                return Ok(());
            }
        }
        changed = true;
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
    let mut changed = false;
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
                let label: String = Input::with_theme(theme)
                    .with_prompt("标签（备注名）")
                    .default(address.clone())
                    .interact_text()?;

                // weight 固定 1.0（跟单比例统一用 copy_factor 调，避免误填）。
                let mut t = Table::new();
                t["address"] = value(address);
                t["weight"] = value(1.0);
                t["label"] = value(label);
                ensure_targets(&mut doc).push(t);
                save_doc(config_path, &doc)?;
                changed = true;
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
                    changed = true;
                    println!("  已删除。");
                }
            }
            _ => {
                if changed {
                    offer_restart(theme)?;
                }
                return Ok(());
            }
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

fn service_menu(config_path: &Path, theme: &ColorfulTheme) -> Result<()> {
    if !service_installed() {
        println!(
            "\n  {} 服务还没安装（所以启动/重启会报 'Unit not found'）。",
            style("ⓘ").cyan()
        );
        let opts = ["安装服务（设为后台常驻 + 开机自启）", "返回"];
        let c = Select::with_theme(theme)
            .with_prompt("服务")
            .items(&opts)
            .default(0)
            .interact()?;
        if c == 0 {
            install_service(config_path)?;
        }
        return Ok(());
    }

    let running = if service_running() { "运行中" } else { "已停止" };
    let actions = [
        "启动", "停止", "重启", "状态", "日志（跟随）", "卸载服务", "返回",
    ];
    let choice = Select::with_theme(theme)
        .with_prompt(format!("服务（当前：{running}）"))
        .items(&actions)
        .default(0)
        .interact()?;
    match choice {
        0 => run_cmd("sudo", &["systemctl", "start", SERVICE]),
        1 => run_cmd("sudo", &["systemctl", "stop", SERVICE]),
        2 => run_cmd("sudo", &["systemctl", "restart", SERVICE]),
        3 => run_cmd("systemctl", &["status", SERVICE, "--no-pager"]),
        4 => run_cmd("journalctl", &["-u", SERVICE, "-f", "--no-pager"]),
        5 => {
            if Confirm::with_theme(theme).with_prompt("确认卸载服务？").default(false).interact()? {
                uninstall_service();
            }
        }
        _ => {}
    }
    Ok(())
}

/// 生成并安装 systemd 单元（自动填当前用户、目录、二进制路径）。
fn install_service(config_path: &Path) -> Result<()> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".into());
    let dir = std::env::current_dir().context("获取当前目录")?;
    let exe = std::env::current_exe().context("定位 pmcopy 程序")?;
    let env_file = dir.join(ENV_PATH);
    let cfg = if config_path.is_absolute() {
        config_path.to_path_buf()
    } else {
        dir.join(config_path)
    };

    let unit = format!(
        "[Unit]\n\
         Description=Polymarket copy-trading bot (pmcopy)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\n\
         [Service]\n\
         Type=simple\n\
         User={user}\n\
         WorkingDirectory={dir}\n\
         EnvironmentFile={env}\n\
         ExecStart={exe} --config {cfg}\n\
         Restart=always\n\
         RestartSec=2\n\n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        dir = dir.display(),
        env = env_file.display(),
        exe = exe.display(),
        cfg = cfg.display(),
    );

    println!("  写入 {UNIT_PATH} (需要 sudo)...");
    let mut child = Command::new("sudo")
        .args(["tee", UNIT_PATH])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("sudo tee 写入单元文件")?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(unit.as_bytes())
        .context("写入单元内容")?;
    child.wait().ok();

    run_cmd("sudo", &["systemctl", "daemon-reload"]);
    run_cmd("sudo", &["systemctl", "enable", SERVICE]);
    println!(
        "  {} 已安装。用「服务 → 启动」开跑。",
        style("✔").green()
    );
    Ok(())
}

fn uninstall_service() {
    run_cmd("sudo", &["systemctl", "disable", "--now", SERVICE]);
    run_cmd("sudo", &["rm", "-f", UNIT_PATH]);
    run_cmd("sudo", &["systemctl", "daemon-reload"]);
    println!("  {} 已卸载服务。", style("✔").green());
}

// ---------------------------------------------------------------------------
// 更新程序
// ---------------------------------------------------------------------------

/// git pull + 重新编译，并刷新已安装到 /usr/local/bin 的二进制。
fn update_self(theme: &ColorfulTheme) -> Result<()> {
    println!("\n  {} 拉取最新代码 (git pull)...", style("==>").cyan());
    run_cmd("git", &["pull", "--ff-only"]);
    println!("  {} 重新编译（可能几分钟）...", style("==>").cyan());
    let ok = Command::new("cargo")
        .args(["build", "--release"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        println!("  {} 编译失败，未更新。", style("✘").red());
        return Ok(());
    }
    if Path::new("/usr/local/bin/pmcopy").exists() {
        // Replace via temp-file + rename so it works even while this menu (or the
        // service) is running the old binary ("Text file busy" otherwise).
        run_cmd(
            "sudo",
            &["cp", "target/release/pmcopy", "/usr/local/bin/pmcopy.new"],
        );
        run_cmd(
            "sudo",
            &["mv", "-f", "/usr/local/bin/pmcopy.new", "/usr/local/bin/pmcopy"],
        );
    }
    println!(
        "  {} 更新完成。重启服务即用上新版本。",
        style("✔").green()
    );
    offer_restart(theme)?;
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

/// 实时日志：服务在跑就跟随完整日志，否则跟随账本（跟单记录）。
fn follow_log(config_path: &Path) -> Result<()> {
    if service_installed() && service_running() {
        println!("\n  跟随服务日志（Ctrl-C 退出）...\n");
        run_cmd("journalctl", &["-u", SERVICE, "-n", "50", "-f", "--no-pager"]);
        return Ok(());
    }
    let doc = load_doc(config_path)?;
    let path = ledger_path(&doc);
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p).ok();
        }
    }
    if !path.exists() {
        std::fs::write(&path, "").ok();
    }
    println!("\n  跟随跟单记录 {}（Ctrl-C 退出）", path.display());
    println!(
        "  {}",
        style("提示：bot 要在别处运行（服务/前台）才会有新记录；装成服务后这里能看完整实时日志").dim()
    );
    run_cmd("tail", &["-n", "20", "-f", path.to_str().unwrap_or("data/copies.jsonl")]);
    Ok(())
}

fn show_ledger(config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    let path = ledger_path(&doc);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            println!("  还没有跟单记录（{}）。", path.display());
            println!(
                "  说明到现在没真正跟过单。检查：① 顶部「服务」是否运行中；\n  ② 是否还在启动窗口对齐期；③ 用「📜 实时日志」看 bot 是否在监听到成交。"
            );
            return Ok(());
        }
    };
    let lines: Vec<&str> = content.lines().collect();
    println!("\n  最近 {} 笔跟单（{}）：", 15.min(lines.len()), path.display());
    for line in lines.iter().rev().take(15).rev() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let tag = if v["submitted"].as_bool().unwrap_or(false) {
                style("已下单").red()
            } else {
                style("模拟").green()
            };
            println!(
                "    {}  [{tag}] {} {} @ {}  (~{} USDC)  本机{}ms  {}",
                v["ts"].as_str().unwrap_or(""),
                v["side"].as_str().unwrap_or(""),
                v["size_shares"],
                v["price"],
                v["usdc"],
                v["proc_ms"],
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
