# polymarket_copy

Low-latency copy-trading bot for [Polymarket](https://polymarket.com), in Rust.
Watches target wallets via on-chain event subscription and mirrors their trades
onto your account, sized proportionally. Defaults to **dry-run** (logs only, no
real orders).

## Install

Needs [Rust](https://rustup.rs) and a Polygon **WebSocket** RPC URL
(`wss://…` — free from [Alchemy](https://www.alchemy.com), Infura, etc.).

```bash
git clone https://github.com/leosysd/polymarket_copy.git
cd polymarket_copy
cargo build --release          # binary at target/release/pmcopy

cp config.example.toml config.toml   # set your target wallet address(es)
cp .env.example .env                 # set PM_WSS_RPC=wss://...
```

## Usage

```bash
# interactive management menu — configure, manage targets, control the
# service, view the ledger, derive keys (recommended; arrow keys to navigate)
./target/release/pmcopy menu

# run the bot directly (dry-run by default)
./target/release/pmcopy

# use a specific config file
./target/release/pmcopy --config /path/to/config.toml

# print CLOB API credentials for .env (live mode only)
./target/release/pmcopy derive-key

# all options
./target/release/pmcopy --help
```

Copy decisions are written to `data/copies.jsonl` — `tail -f` it to watch.

**Key settings** (`config.toml`):

| Setting | Meaning |
|---|---|
| `mode` | `dry_run` (default) or `live` |
| `copy_factor` | size multiplier — `0.25` = follow at 25% of the target's size |
| `max_slippage` | price offset to cross — `0.02` → target 0.50 fills at 0.52 |
| `order_type` | `FAK` (fill now, cancel rest), `FOK`, or `GTC` |
| `min_order_usdc` / `max_order_usdc` | per-copy floor / ceiling |
| `[[targets]]` | wallet `address` to follow (+ optional `weight`, `label`) |

To go **live**, set `mode = "live"` and put `PM_PRIVATE_KEY` in `.env`
(API credentials are auto-derived). Start with a small `copy_factor`.

### Run as a service (VPS)

```bash
sudo cp deploy/pmcopy.service /etc/systemd/system/
sudo nano /etc/systemd/system/pmcopy.service   # set User=
sudo systemctl enable --now pmcopy
journalctl -u pmcopy -f                         # view logs
```

## Uninstall

```bash
# if installed as a service:
sudo systemctl disable --now pmcopy
sudo rm /etc/systemd/system/pmcopy.service
sudo systemctl daemon-reload

# remove the bot
rm -rf ~/polymarket_copy
```

There is nothing else to clean up — no system files, no database. Just remember
to revoke the CLOB API key if you created one.

## License

MIT — provided as-is, no warranty. Copy-trading is risky; you can lose money.
