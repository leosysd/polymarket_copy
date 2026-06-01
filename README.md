# polymarket_copy

Polymarket 跟单机器人（Rust）。链上实时监听目标钱包的成交，按比例自动跟单。
默认 **dry_run**（只记录、不真实下单）。

## 安装

需要 [Rust](https://rustup.rs) 和一个 Polygon 的 **WebSocket** RPC 地址
（`wss://…`，[Alchemy](https://www.alchemy.com) 免费档即可）。

```bash
git clone https://github.com/leosysd/polymarket_copy.git
cd polymarket_copy
cargo build --release                 # 编译，产物在 target/release/pmcopy

cp config.example.toml config.toml    # 填目标钱包地址、跟单比例等
cp .env.example .env                  # 填 PM_WSS_RPC=wss://...
```

## 使用

```bash
./target/release/pmcopy menu     # 交互式中文菜单（推荐：改配置、管目标、看账本、控服务）
./target/release/pmcopy          # 直接运行（默认 dry_run，只记录不下单）
```

开实盘：菜单里把 `mode` 改成 `live`，并在 `.env` 填 `PM_PRIVATE_KEY`（API 凭证会自动派生）。
建议先用很小的 `copy_factor` 试。

可选：作为后台服务常驻（VPS）

```bash
sudo cp deploy/pmcopy.service /etc/systemd/system/
sudo nano /etc/systemd/system/pmcopy.service    # 改 User=
sudo systemctl enable --now pmcopy
```

## 卸载

```bash
# 如果装了后台服务：
sudo systemctl disable --now pmcopy
sudo rm /etc/systemd/system/pmcopy.service
sudo systemctl daemon-reload

# 删掉程序目录：
rm -rf ~/polymarket_copy
```

没有别的系统文件或数据库要清理。如果创建过 CLOB API key，记得去 Polymarket 吊销。

---

MIT 许可，按现状提供，不作担保。跟单有风险，可能亏损。
