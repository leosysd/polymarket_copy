# polymarket_copy

A **low-latency copy-trading (跟单) bot for [Polymarket](https://polymarket.com),
written in Rust.** Built for fast markets like **BTC 5-minute up/down**.

It watches one or more *target* wallets via **on-chain event subscription**, and
whenever a target fills a trade it mirrors that trade onto **your** account —
sized **proportionally** to the target's trade and clamped by your own risk limits.

It runs in **`dry_run` mode by default**: it monitors and logs exactly what it
*would* do, placing **no real orders**, until you explicitly switch to `live`.

---

## Why on-chain (not the Data-API)

The obvious approach — polling Polymarket's Data-API for each target's activity —
**does not work for fast markets**. Measured behaviour: the Data-API lags
**1–3 minutes** behind reality (CDN cache + indexing). On a 300-second market the
target's entry wouldn't even be visible until the market is nearly over.

So this bot subscribes to **Polygon `OrderFilled` logs over a WebSocket RPC**.
Fills arrive at ~block time (single-digit seconds) — verified live: target fills
were observed **~1 second** after they happened.

```
 ┌─────────────────────┐  OrderFilled logs   ┌──────────┐  proportional  ┌────────────┐
 │ Polygon node (wss)  │ ──────────────────▶ │  sizing  │ ─────────────▶ │  executor  │
 │ subscribe_logs()    │  maker == target    │  + caps  │   CopyOrder    │ dry / live │
 └─────────────────────┘                     └──────────┘                └─────┬──────┘
                                                                               │
                          dry_run → JSONL ledger          live → signed CLOB /order
```

1. **Monitor** (`monitor.rs`) — subscribes to fill logs on the Polymarket
   exchange contract, filtered to events where the order **maker** is one of your
   targets. Decodes `(side, tokenId, price, shares, usdc)` directly from the log.
   Auto-reconnects on disconnect; dedups by `(tx_hash, log_index)`.
2. **Sizing** (`sizing.rs`) — `our_shares = target_shares × copy_factor ×
   target.weight`, clamped by `min_order_usdc` / `max_order_usdc`. A
   marketable-limit price is derived from the target's price ± `max_slippage_bps`.
3. **Execute** (`executor/`)
   - `dry_run`: appends the decision to a JSONL ledger and logs it. No writes.
   - `live`: builds the EIP-712 `Order`, signs it, and submits to the CLOB
     `/order` endpoint with L2 (HMAC) auth.

Because we subscribe to **new** logs only, there's no history to replay — a
restart never re-copies old trades.

---

## Requirements

- **Rust** (stable).
- A **Polygon WebSocket RPC** endpoint (`wss://…`). Free tiers work:
  [Alchemy](https://www.alchemy.com) (`wss://polygon-mainnet.g.alchemy.com/v2/KEY`),
  Infura, QuickNode, etc. A plain `https` RPC **cannot** do subscriptions.

---

## Quick start (dry run)

```bash
# 1. Build
cargo build --release

# 2. Configure
cp config.example.toml config.toml      # set your target wallet address(es)
cp .env.example .env                     # set PM_WSS_RPC=wss://...

# 3. Run — monitor + log only, no orders
./target/release/pmcopy --config config.toml
```

Watch decisions accumulate in the ledger:

```bash
tail -f data/copies.jsonl
```

---

## Configuration

Non-secret settings live in **`config.toml`** (see `config.example.toml`);
secrets — including the RPC URL — live in **`.env`** (see `.env.example`). Both
are gitignored.

| Key | Meaning |
|---|---|
| `mode` | `"dry_run"` (default, safe) or `"live"` |
| `copy_factor` | Global size multiplier (e.g. `0.10` = follow at 10%) |
| `min_order_usdc` | Skip copies smaller than this (dust filter) |
| `max_order_usdc` | Hard ceiling on USDC per single copy |
| `only_buys` | `true` = mirror entries only, ignore the target's exits |
| `max_slippage_bps` | Marketable-limit slippage allowance (100 = 1%) |
| `order_type` | `FAK` (fill now, cancel rest — default), `FOK` (all-or-nothing), `GTC` (leftover rests) |
| `[[targets]]` | `address`, optional `weight` (per-target multiplier) and `label` |
| `endpoints.log_sources` | Contracts whose fills to watch (has a verified default) |

`PM_WSS_RPC` (in `.env`) is **required** — it's the Polygon `wss://` endpoint.

The target `address` is the wallet that appears as the order **maker** in
on-chain fills (your Polymarket trading/proxy address). You can find active
traders from the Polymarket UI or the public Data-API (`/trades`).

---

## Going live

> ⚠️ **Live mode places real orders with real funds on Polygon mainnet.**
> Start with a tiny `copy_factor` and a low `max_order_usdc`, and verify your
> first fills manually.

1. Set `mode = "live"` in `config.toml`.
2. Fill in `.env`:

   | Var | What |
   |---|---|
   | `PM_WSS_RPC` | Polygon `wss://` endpoint (**required**, all modes) |
   | `PM_PRIVATE_KEY` | EOA private key that signs orders (**required** for live) |
   | `PM_API_KEY` / `PM_API_SECRET` / `PM_API_PASSPHRASE` | CLOB creds (**optional** — auto-derived if blank) |
   | `PM_FUNDER_ADDRESS` | Fund-holding address (proxy/safe); blank for plain EOA |
   | `PM_SIGNATURE_TYPE` | `0` EOA, `1` email/magic proxy, `2` browser-wallet safe |

3. **CLOB API credentials are handled for you.** If they're blank the bot derives
   them from `PM_PRIVATE_KEY` at startup (L1 `ClobAuth` signing — no
   `py-clob-client` needed). To print them yourself:

   ```bash
   ./target/release/pmcopy derive-key
   ```

### Live-trading status (read this)

- **Monitoring** is fully verified against live on-chain BTC 5-minute trades.
- **Order placement** uses the EIP-712 CTF-Exchange `Order` schema + L2 auth, and
  the credential-derivation half is verified against the live CLOB. However, a
  real funded fill has **not** been confirmed end-to-end, and the EIP-712
  `endpoints.exchange` (verifying contract) may need to match Polymarket's current
  deployment for the markets you trade. **Validate with a tiny size before
  trusting it.** Also note: your account must already be funded and have the
  on-chain USDC/CTF approvals (done automatically when you deposit via the UI).

---

## Project layout

```
src/
├── main.rs            # CLI + the subscribe→size→execute loop, shutdown
├── config.rs          # TOML config + .env secrets, validation
├── models.rs          # TargetTrade (decoded fill) + the CopyOrder we derive
├── monitor.rs         # on-chain OrderFilled WS subscription + decode + reconnect
├── sizing.rs          # proportional sizing + caps + slippage
├── state.rs           # persisted dedup set (atomic JSON)
├── executor/
│   ├── mod.rs         # OrderExecutor trait + ExecOutcome
│   ├── dry_run.rs     # logs decisions to a JSONL ledger
│   └── clob.rs        # signs + submits real orders
└── clob/
    ├── signing.rs     # EIP-712 Order + ClobAuth construction + signing (alloy)
    ├── auth.rs        # L2 HMAC-SHA256 POLY_* headers
    ├── keys.rs        # derive/create API credentials from the key (L1 auth)
    └── client.rs      # authenticated POST /order
```

---

## Deploying to a VPS

A sample systemd unit is in [`deploy/pmcopy.service`](deploy/pmcopy.service).

```bash
# on the VPS, as your user:
git clone https://github.com/leosysd/polymarket_copy.git && cd polymarket_copy
cargo build --release
cp config.example.toml config.toml && $EDITOR config.toml
cp .env.example .env && $EDITOR .env            # PM_WSS_RPC=...

sudo cp deploy/pmcopy.service /etc/systemd/system/
sudo $EDITOR /etc/systemd/system/pmcopy.service  # set User= and WorkingDirectory=
sudo systemctl daemon-reload
sudo systemctl enable --now pmcopy
journalctl -u pmcopy -f
```

---

## Safety notes & disclaimer

- Defaults to `dry_run`; you must opt into `live` and supply credentials.
- Secrets (incl. the RPC URL) are read only from `.env` and never committed.
- Each handled fill is marked "seen" after the attempt, so a reconnect/restart
  never double-submits (it also won't auto-retry a failed submit — by design, to
  avoid accidental double-fills).
- This software is provided **as-is, with no warranty**. Copy-trading is risky and
  you can lose money. You are responsible for your own keys, funds, and for
  validating live behaviour with small sizes first. Not financial advice.

## License

MIT
