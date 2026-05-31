# polymarket_copy

A **copy-trading (跟单) bot for [Polymarket](https://polymarket.com), written in Rust.**

It watches one or more *target* wallets, and whenever a target makes a trade it
mirrors that trade onto **your** account — sized **proportionally** to the
target's trade and clamped by your own risk limits.

It runs in **`dry_run` mode by default**: it monitors and logs exactly what it
*would* do, placing **no real orders**, until you explicitly switch to `live`.

---

## How it works

```
 ┌──────────────┐   poll /activity    ┌──────────┐   proportional    ┌────────────┐
 │ Data-API     │ ──────────────────▶ │  sizing  │ ────────────────▶ │  executor  │
 │ (per target) │   new TRADE events  │  + caps  │   CopyOrder       │ dry / live │
 └──────────────┘                     └──────────┘                   └─────┬──────┘
                                                                           │
                          dry_run → JSONL ledger        live → signed CLOB /order
```

1. **Monitor** — polls `GET {data_api}/activity?user=<target>` for each target on
   an interval and detects *new* `TRADE` events (deduped by a persisted state file,
   so a restart never replays history).
2. **Sizing** — `our_shares = target_shares × copy_factor × target.weight`, then
   clamped by `min_order_usdc` / `max_order_usdc`. A marketable-limit price is
   derived from the target's fill price plus `max_slippage_bps` so the order crosses.
3. **Execute**
   - `dry_run`: appends the decision to a JSONL ledger and logs it. No network writes.
   - `live`: builds the EIP-712 `Order`, signs it with your key, and submits it to
     the CLOB `/order` endpoint with L2 (HMAC) authentication.

### First run is safe by design

On the very first run for a fresh state file the bot **bootstraps**: it records all
existing history as "seen" *without trading*, then only copies trades that happen
**after** it started. You never get blasted with a backlog of old fills.

---

## Quick start (dry run)

```bash
# 1. Build
cargo build --release

# 2. Configure
cp config.example.toml config.toml
$EDITOR config.toml          # set your target wallet address(es)

# 3. Run — monitor + log only, no orders
./target/release/pmcopy --config config.toml
```

Run a single poll cycle and exit (handy for testing):

```bash
./target/release/pmcopy --config config.toml --once
```

Watch decisions accumulate in the ledger:

```bash
tail -f data/copies.jsonl
```

---

## Configuration

Non-secret settings live in **`config.toml`** (see `config.example.toml`);
secrets live in **`.env`** (see `.env.example`). Both are gitignored.

| Key | Meaning |
|---|---|
| `mode` | `"dry_run"` (default, safe) or `"live"` |
| `poll_interval_secs` | Seconds between polls of each target |
| `copy_factor` | Global size multiplier (e.g. `0.10` = follow at 10%) |
| `min_order_usdc` | Skip copies smaller than this (dust filter) |
| `max_order_usdc` | Hard ceiling on USDC per single copy |
| `only_buys` | `true` = mirror entries only, ignore the target's exits |
| `max_slippage_bps` | Marketable-limit slippage allowance (100 = 1%) |
| `[[targets]]` | `address`, optional `weight` (per-target multiplier) and `label` |

Find target wallet addresses on a trader's Polymarket profile, or from the public
Data-API (`/trades`, `/activity`). The address is the **proxy wallet** that shows up
as `proxyWallet` in the activity feed.

---

## Going live

> ⚠️ **Live mode places real orders with real funds on Polygon mainnet.**
> Start with a tiny `copy_factor` and a low `max_order_usdc`, and verify your first
> fills manually.

1. Set `mode = "live"` in `config.toml`.
2. Fill in `.env` (copy from `.env.example`):

   | Var | What |
   |---|---|
   | `PM_PRIVATE_KEY` | EOA private key that signs orders |
   | `PM_API_KEY` / `PM_API_SECRET` / `PM_API_PASSPHRASE` | CLOB L2 credentials |
   | `PM_FUNDER_ADDRESS` | Fund-holding address (proxy/safe); blank for plain EOA |
   | `PM_SIGNATURE_TYPE` | `0` EOA, `1` email/magic proxy, `2` browser-wallet safe |

3. **Getting CLOB API credentials**: derive them once with the official
   [`py-clob-client`](https://github.com/Polymarket/py-clob-client)
   (`client.create_or_derive_api_creds()`) using the same private key, or copy them
   from the Polymarket UI. The bot expects them ready-made.

The order is signed as the Polymarket **CTF Exchange** EIP-712 `Order` struct
(domain `Polymarket CTF Exchange` / v1 / chainId 137) and submitted as a `GTC`
marketable-limit order, matching the schema used by `py-clob-client`.

---

## Project layout

```
src/
├── main.rs            # CLI + the poll→size→execute loop, bootstrap, shutdown
├── config.rs          # TOML config + .env secrets, live-mode validation
├── models.rs          # Data-API activity schema + the CopyOrder we derive
├── monitor.rs         # Data-API activity polling
├── sizing.rs          # proportional sizing + caps + slippage
├── state.rs           # persisted dedup set (atomic JSON), bootstrap flag
├── executor/
│   ├── mod.rs         # OrderExecutor trait + ExecOutcome
│   ├── dry_run.rs     # logs decisions to a JSONL ledger
│   └── clob.rs        # signs + submits real orders
└── clob/
    ├── signing.rs     # EIP-712 Order construction + signing (alloy)
    ├── auth.rs        # L2 HMAC-SHA256 POLY_* headers
    └── client.rs      # authenticated POST /order
```

---

## Safety notes & disclaimer

- Defaults to `dry_run`; you must opt into `live` and supply credentials.
- Secrets are read only from the environment / `.env` and are never committed.
- Each handled trade is marked "seen" after the attempt, so a crash/restart will
  **not** re-submit it (it also won't auto-retry a failed submit — by design, to
  avoid accidental double-fills).
- This software is provided **as-is, with no warranty**. Copy-trading is risky and
  you can lose money. You are responsible for your own keys, funds, and for
  validating live behaviour with small sizes first. Not financial advice.

## License

MIT
