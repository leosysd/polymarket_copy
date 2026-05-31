//! EIP-712 order construction and signing for the Polymarket CTF Exchange.
//!
//! Mirrors the order schema used by `py-clob-client`: a 12-field `Order` struct
//! signed under the "Polymarket CTF Exchange" domain on Polygon.

use crate::models::Side;
use alloy::primitives::{Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use alloy::sol;
use alloy::sol_types::{eip712_domain, SolStruct};
use anyhow::{Context, Result};
use rand::Rng;
use serde::Serialize;

sol! {
    #[allow(missing_docs)]
    struct Order {
        uint256 salt;
        address maker;
        address signer;
        address taker;
        uint256 tokenId;
        uint256 makerAmount;
        uint256 takerAmount;
        uint256 expiration;
        uint256 nonce;
        uint256 feeRateBps;
        uint8 side;
        uint8 signatureType;
    }
}

/// The inputs needed to build one signed order.
pub struct OrderInputs {
    pub token_id: String,
    pub side: Side,
    /// Base units (6 decimals): what the maker gives.
    pub maker_amount: u128,
    /// Base units (6 decimals): what the maker expects to receive.
    pub taker_amount: u128,
    pub maker: Address,
    pub signer: Address,
    pub signature_type: u8,
    pub fee_rate_bps: u64,
}

/// The JSON object posted as `order` in the CLOB `/order` request body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedOrderPayload {
    pub salt: String,
    pub maker: String,
    pub signer: String,
    pub taker: String,
    pub token_id: String,
    pub maker_amount: String,
    pub taker_amount: String,
    pub expiration: String,
    pub nonce: String,
    pub fee_rate_bps: String,
    pub side: String,
    pub signature_type: u8,
    pub signature: String,
}

pub struct OrderSigner {
    signer: PrivateKeySigner,
    chain_id: u64,
    exchange: Address,
}

impl OrderSigner {
    pub fn new(private_key: &str, chain_id: u64, exchange: &str) -> Result<OrderSigner> {
        let signer: PrivateKeySigner =
            private_key.trim().parse().context("parsing PM_PRIVATE_KEY")?;
        let exchange: Address = exchange.parse().context("parsing exchange address")?;
        Ok(OrderSigner {
            signer,
            chain_id,
            exchange,
        })
    }

    pub fn address(&self) -> Address {
        self.signer.address()
    }

    pub fn sign(&self, inp: &OrderInputs) -> Result<SignedOrderPayload> {
        let salt: u64 = rand::thread_rng().gen();
        let token_id =
            U256::from_str_radix(inp.token_id.trim(), 10).context("parsing tokenId as integer")?;

        let order = Order {
            salt: U256::from(salt),
            maker: inp.maker,
            signer: inp.signer,
            taker: Address::ZERO,
            tokenId: token_id,
            makerAmount: U256::from(inp.maker_amount),
            takerAmount: U256::from(inp.taker_amount),
            expiration: U256::ZERO,
            nonce: U256::ZERO,
            feeRateBps: U256::from(inp.fee_rate_bps),
            side: inp.side.as_u8(),
            signatureType: inp.signature_type,
        };

        let domain = eip712_domain! {
            name: "Polymarket CTF Exchange",
            version: "1",
            chain_id: self.chain_id,
            verifying_contract: self.exchange,
        };

        let hash: B256 = order.eip712_signing_hash(&domain);
        let sig = self.signer.sign_hash_sync(&hash).context("signing order hash")?;

        // r || s || v, with v normalised to {27, 28} as the exchange expects.
        let mut bytes = [0u8; 65];
        bytes[..32].copy_from_slice(&sig.r().to_be_bytes::<32>());
        bytes[32..64].copy_from_slice(&sig.s().to_be_bytes::<32>());
        bytes[64] = 27 + sig.v() as u8;
        let signature = format!("0x{}", hex::encode(bytes));

        Ok(SignedOrderPayload {
            salt: salt.to_string(),
            maker: inp.maker.to_checksum(None),
            signer: inp.signer.to_checksum(None),
            taker: Address::ZERO.to_checksum(None),
            token_id: inp.token_id.clone(),
            maker_amount: inp.maker_amount.to_string(),
            taker_amount: inp.taker_amount.to_string(),
            expiration: "0".to_string(),
            nonce: "0".to_string(),
            fee_rate_bps: inp.fee_rate_bps.to_string(),
            side: inp.side.as_str().to_string(),
            signature_type: inp.signature_type,
            signature,
        })
    }
}
