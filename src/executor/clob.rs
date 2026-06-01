//! Live executor: signs and submits real orders through the Polymarket CLOB.

use super::{ExecOutcome, OrderExecutor};
use crate::clob::{ClobClient, L2Creds, OrderInputs, OrderSigner};
use crate::config::{Endpoints, Secrets};
use crate::models::{CopyOrder, Side};
use alloy::primitives::Address;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use reqwest::Client;

pub struct ClobExecutor {
    signer: OrderSigner,
    client: ClobClient,
    /// The fund-holding address ("maker"); equals the signer for EOA accounts.
    maker: Address,
    signature_type: u8,
    /// Time-in-force sent to the CLOB (FAK / FOK / GTC / GTD).
    order_type: String,
}

impl ClobExecutor {
    pub fn new(
        http: Client,
        endpoints: &Endpoints,
        secrets: &Secrets,
        order_type: String,
    ) -> Result<ClobExecutor> {
        let pk = secrets
            .private_key
            .as_ref()
            .ok_or_else(|| anyhow!("missing PM_PRIVATE_KEY"))?;
        let signer = OrderSigner::new(pk, endpoints.chain_id, &endpoints.exchange)?;
        let signer_addr = signer.address();

        let maker = match secrets.funder_address.as_deref() {
            Some(a) if !a.trim().is_empty() => {
                a.trim().parse().context("parsing PM_FUNDER_ADDRESS")?
            }
            _ => signer_addr,
        };

        let creds = L2Creds {
            api_key: secrets.api_key.clone().unwrap_or_default(),
            secret: secrets.api_secret.clone().unwrap_or_default(),
            passphrase: secrets.api_passphrase.clone().unwrap_or_default(),
            address: signer_addr.to_checksum(None),
        };
        let client = ClobClient::new(http, endpoints.clob.clone(), creds);

        Ok(ClobExecutor {
            signer,
            client,
            maker,
            signature_type: secrets.signature_type,
            order_type,
        })
    }
}

/// USDC and conditional tokens both use 6 decimals on Polymarket.
fn to_base_units(amount: f64) -> u128 {
    (amount * 1_000_000.0).round().max(0.0) as u128
}

#[async_trait]
impl OrderExecutor for ClobExecutor {
    async fn execute(&self, order: &CopyOrder) -> Result<ExecOutcome> {
        let shares = order.size_shares;
        let notional = order.price * shares;

        // BUY: pay USDC (maker) to receive shares (taker).
        // SELL: give shares (maker) to receive USDC (taker).
        let (maker_amount, taker_amount) = match order.side {
            Side::Buy => (to_base_units(notional), to_base_units(shares)),
            Side::Sell => (to_base_units(shares), to_base_units(notional)),
        };

        let inputs = OrderInputs {
            token_id: order.token_id.clone(),
            side: order.side,
            maker_amount,
            taker_amount,
            maker: self.maker,
            signer: self.signer.address(),
            signature_type: self.signature_type,
            fee_rate_bps: 0,
        };

        let payload = self.signer.sign(&inputs)?;
        // Marketable-limit at our slipped price. With FAK, whatever crosses now
        // fills and the remainder is cancelled (no resting order left behind).
        let resp = self.client.post_order(&payload, &self.order_type).await?;

        Ok(ExecOutcome {
            submitted: true,
            detail: format!("CLOB accepted: {resp}"),
        })
    }

    fn label(&self) -> &'static str {
        "live"
    }
}
