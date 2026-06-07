//! Live executor: creates, signs and submits orders via the official Polymarket
//! Rust SDK (`polymarket_client_sdk_v2`) against the CLOB v2 endpoint. This
//! replaces the hand-rolled EIP-712 signing, which produced V1-format orders the
//! V2 CLOB rejected ("Invalid order payload").

use super::{ExecOutcome, OrderExecutor};
use crate::config::{OrderStyle, Secrets};
use crate::models::{CopyOrder, Side};
use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use polymarket_client_sdk_v2::auth::state::Authenticated;
use polymarket_client_sdk_v2::auth::Normal;
use polymarket_client_sdk_v2::clob::types::request::OrderBookSummaryRequest;
use polymarket_client_sdk_v2::clob::types::response::PostOrderResponse;
use polymarket_client_sdk_v2::clob::types::{Amount, OrderType, SignatureType, Side as PmSide};
use polymarket_client_sdk_v2::clob::{Client, Config};
use polymarket_client_sdk_v2::types::{Decimal, U256};
use polymarket_client_sdk_v2::POLYGON;
use std::str::FromStr;

// Note: clob-v2.polymarket.com 301-redirects (POST -> GET -> 405); the real
// host that accepts authenticated POST /order is clob.polymarket.com.
const CLOB_V2_HOST: &str = "https://clob.polymarket.com";
const MAKER_CROSS_RETRIES: u32 = 1;

pub struct ClobExecutor {
    client: Client<Authenticated<Normal>>,
    signer: PrivateKeySigner,
    order_style: OrderStyle,
    order_type: OrderType,
}

impl ClobExecutor {
    pub async fn new(
        secrets: &Secrets,
        order_style: OrderStyle,
        order_type: &str,
    ) -> Result<ClobExecutor> {
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
            _ => OrderType::FAK,
        };

        Ok(ClobExecutor {
            client,
            signer,
            order_style,
            order_type,
        })
    }
}

#[async_trait]
impl OrderExecutor for ClobExecutor {
    async fn execute(&self, order: &CopyOrder) -> Result<ExecOutcome> {
        let token_id =
            U256::from_str_radix(order.token_id.trim(), 10).context("token_id -> U256")?;

        // Warm the SDK's tick-size + neg-risk caches concurrently (cold token =
        // each 5-minute window rotates token ids). Real API values; a hit if warm.
        let _ = tokio::join!(self.client.tick_size(token_id), self.client.neg_risk(token_id));

        let shares = Decimal::from_str(&format!("{:.2}", order.size_shares))
            .context("shares -> Decimal")?;
        let price = Decimal::from_str(&price_string(order.price))
            .context("price -> Decimal")?;
        let pm_side = match order.side {
            Side::Buy => PmSide::Buy,
            Side::Sell => PmSide::Sell,
        };

        let (resp, submitted_price) = match self.order_style {
            OrderStyle::Market => {
                let resp = self.client
                    .market_order()
                    .token_id(token_id)
                    .side(pm_side)
                    .price(price)
                    .amount(Amount::shares(shares)?)
                    .order_type(self.order_type.clone())
                    .build_sign_and_post(&self.signer)
                    .await?;
                (resp, price)
            }
            OrderStyle::Maker => {
                self.execute_maker(token_id, pm_side, shares, price).await?
            }
        };

        let making = resp.making_amount.to_string().parse::<f64>().unwrap_or(0.0);
        let taking = resp.taking_amount.to_string().parse::<f64>().unwrap_or(0.0);
        let (filled_shares, filled_usdc) = match order.side {
            Side::Buy => (taking, making),
            Side::Sell => (making, taking),
        };

        Ok(ExecOutcome {
            // Reflect the order-level result, not just "HTTP 200".
            submitted: resp.success,
            submitted_price: decimal_to_f64(submitted_price),
            filled_shares: if resp.success { filled_shares } else { 0.0 },
            filled_usdc: if resp.success { filled_usdc } else { 0.0 },
            accounted_usdc: if !resp.success {
                0.0
            } else if self.order_style == OrderStyle::Maker {
                order.size_shares * decimal_to_f64(submitted_price)
            } else {
                filled_usdc
            },
            detail: format!(
                "CLOB v2 {}: success={} status={} price={} filled={:.2} shares/{:.2} USDC id={}",
                self.order_style.as_str(),
                resp.success,
                resp.status,
                submitted_price,
                filled_shares,
                filled_usdc,
                resp.order_id
            ),
        })
    }

    fn label(&self) -> &'static str {
        "live"
    }
}

impl ClobExecutor {
    async fn execute_maker(
        &self,
        token_id: U256,
        side: PmSide,
        shares: Decimal,
        desired_price: Decimal,
    ) -> Result<(PostOrderResponse, Decimal)> {
        let mut last_err: Option<anyhow::Error> = None;
        for extra_ticks in 0..=MAKER_CROSS_RETRIES {
            let price = self
                .passive_maker_price(token_id, side, desired_price, extra_ticks)
                .await?;
            match self.post_maker_order(token_id, side, shares, price).await {
                Ok(resp) => return Ok((resp, price)),
                Err(e) if is_crosses_book_error(&e) && extra_ticks < MAKER_CROSS_RETRIES => {
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("maker order failed")))
    }

    async fn post_maker_order(
        &self,
        token_id: U256,
        side: PmSide,
        shares: Decimal,
        price: Decimal,
    ) -> Result<PostOrderResponse> {
        Ok(self
            .client
            .limit_order()
            .token_id(token_id)
            .side(side)
            .price(price)
            .size(shares)
            .order_type(OrderType::GTC)
            .post_only(true)
            .build_sign_and_post(&self.signer)
            .await?)
    }

    async fn passive_maker_price(
        &self,
        token_id: U256,
        side: PmSide,
        desired_price: Decimal,
        extra_ticks: u32,
    ) -> Result<Decimal> {
        let book = self
            .client
            .order_book(&OrderBookSummaryRequest::builder().token_id(token_id).build())
            .await
            .context("fetching order book for maker pricing")?;
        let tick = book.tick_size.as_decimal();
        let away = tick * Decimal::from(extra_ticks + 1);
        let min_price = tick;
        let max_price = Decimal::ONE - tick;

        let price = match side {
            PmSide::Buy => {
                let Some(best_ask) = book.asks.iter().map(|l| l.price).min() else {
                    return Ok(desired_price.clamp(min_price, max_price));
                };
                let cap = best_ask - away;
                if cap < min_price {
                    return Err(anyhow!(
                        "no passive BUY price available: best_ask={best_ask} tick={tick}"
                    ));
                }
                if desired_price < cap { desired_price } else { cap }
            }
            PmSide::Sell => {
                let Some(best_bid) = book.bids.iter().map(|l| l.price).max() else {
                    return Ok(desired_price.clamp(min_price, max_price));
                };
                let floor = best_bid + away;
                if floor > max_price {
                    return Err(anyhow!(
                        "no passive SELL price available: best_bid={best_bid} tick={tick}"
                    ));
                }
                if desired_price > floor { desired_price } else { floor }
            }
            PmSide::Unknown => return Err(anyhow!("cannot price maker order for unknown side")),
            _ => return Err(anyhow!("cannot price maker order for unsupported side")),
        };

        Ok(price.clamp(min_price, max_price))
    }
}

fn is_crosses_book_error(error: &anyhow::Error) -> bool {
    error.to_string().contains("post-only order: order crosses book")
}

fn decimal_to_f64(value: Decimal) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
}

fn price_string(price: f64) -> String {
    let mut s = format!("{price:.4}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.push('0');
    }
    s
}
