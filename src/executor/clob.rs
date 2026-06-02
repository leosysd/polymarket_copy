//! Live executor: creates, signs and submits orders via the official Polymarket
//! Rust SDK (`polymarket_client_sdk_v2`) against the CLOB v2 endpoint. This
//! replaces the hand-rolled EIP-712 signing, which produced V1-format orders the
//! V2 CLOB rejected ("Invalid order payload").

use super::{ExecOutcome, OrderExecutor};
use crate::config::Secrets;
use crate::models::{CopyOrder, Side};
use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use polymarket_client_sdk_v2::auth::state::Authenticated;
use polymarket_client_sdk_v2::auth::Normal;
use polymarket_client_sdk_v2::clob::types::{OrderType, SignatureType, Side as PmSide};
use polymarket_client_sdk_v2::clob::{Client, Config};
use polymarket_client_sdk_v2::types::{Decimal, U256};
use polymarket_client_sdk_v2::POLYGON;
use std::str::FromStr;

// Note: clob-v2.polymarket.com 301-redirects (POST -> GET -> 405); the real
// host that accepts authenticated POST /order is clob.polymarket.com.
const CLOB_V2_HOST: &str = "https://clob.polymarket.com";

pub struct ClobExecutor {
    client: Client<Authenticated<Normal>>,
    signer: PrivateKeySigner,
    order_type: OrderType,
}

impl ClobExecutor {
    pub async fn new(secrets: &Secrets, order_type: &str) -> Result<ClobExecutor> {
        let pk = secrets
            .private_key
            .as_ref()
            .ok_or_else(|| anyhow!("missing PM_PRIVATE_KEY"))?;
        let signer = PrivateKeySigner::from_str(pk.trim())
            .context("parsing PM_PRIVATE_KEY")?
            .with_chain_id(Some(POLYGON));

        // 0=EOA, 1=Proxy(email/magic), 2=GnosisSafe, 3=Poly1271(V2 deposit wallet).
        let sig_type = match secrets.signature_type {
            1 => SignatureType::Proxy,
            2 => SignatureType::GnosisSafe,
            3 => SignatureType::Poly1271,
            _ => SignatureType::Eoa,
        };

        let mut builder = Client::new(CLOB_V2_HOST, Config::default())?
            .authentication_builder(&signer)
            .signature_type(sig_type);
        if let Some(f) = secrets
            .funder_address
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let addr = Address::from_str(f.trim()).context("parsing PM_FUNDER_ADDRESS")?;
            builder = builder.funder(addr);
        }
        let client = builder.authenticate().await?;

        let order_type = match order_type.to_uppercase().as_str() {
            "FOK" => OrderType::FOK,
            "GTC" => OrderType::GTC,
            _ => OrderType::FAK,
        };

        Ok(ClobExecutor {
            client,
            signer,
            order_type,
        })
    }
}

#[async_trait]
impl OrderExecutor for ClobExecutor {
    async fn execute(&self, order: &CopyOrder) -> Result<ExecOutcome> {
        let side = match order.side {
            Side::Buy => PmSide::Buy,
            Side::Sell => PmSide::Sell,
        };
        // Polymarket needs BUY notional (price*size) to be <= 2 decimals: price is
        // 2-dec, so round size to an integer (matches the working JY bot).
        let limit = (order.price * 100.0).round() / 100.0;
        let order_shares = order.size_shares.round().max(1.0);
        let price = Decimal::from_str(&format!("{limit:.2}")).context("price -> Decimal")?;
        let size = Decimal::from_str(&format!("{order_shares:.0}")).context("size -> Decimal")?;
        let token_id =
            U256::from_str_radix(order.token_id.trim(), 10).context("token_id -> U256")?;

        // Warm the SDK's tick-size + neg-risk caches concurrently. build_sign_and_post
        // needs both; on a cold token (each 5-minute window rotates token ids) it
        // would otherwise fetch them in two *sequential* round trips. Warming them
        // in parallel first turns that into one. Real API values (no hardcoding);
        // a hit if already warm. Errors are ignored — build_sign_and_post will
        // fetch and surface the real error if these failed.
        let _ = tokio::join!(self.client.tick_size(token_id), self.client.neg_risk(token_id));

        // build + sign + post in one call (handles tick size, neg-risk, contracts).
        let resp = self
            .client
            .limit_order()
            .token_id(token_id)
            .size(size)
            .price(price)
            .side(side)
            .order_type(self.order_type.clone())
            .build_sign_and_post(&self.signer)
            .await?;

        Ok(ExecOutcome {
            // Reflect the order-level result, not just "HTTP 200": a 200 with
            // success=false (e.g. unmatched) should not read as submitted.
            submitted: resp.success,
            detail: format!("CLOB v2 accepted: {resp:?}"),
        })
    }

    fn label(&self) -> &'static str {
        "live"
    }
}
