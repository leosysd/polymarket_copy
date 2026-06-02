//! EIP-712 signing of the `ClobAuth` struct (L1 auth), used to create/derive the
//! Polymarket CLOB API credentials. Live order signing is done by the official
//! `polymarket_client_sdk_v2` in the executor, not here.

use alloy::primitives::{Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use alloy::sol;
use alloy::sol_types::{eip712_domain, SolStruct};
use anyhow::{Context, Result};

sol! {
    // L1 auth struct used to derive/create CLOB API credentials.
    #[allow(missing_docs)]
    struct ClobAuth {
        address address;
        string timestamp;
        uint256 nonce;
        string message;
    }
}

const CLOB_AUTH_MESSAGE: &str = "This message attests that I control the given wallet";

/// r || s || v, with v normalised to {27, 28} as Polymarket expects.
fn encode_signature(sig: &alloy::primitives::Signature) -> String {
    let mut bytes = [0u8; 65];
    bytes[..32].copy_from_slice(&sig.r().to_be_bytes::<32>());
    bytes[32..64].copy_from_slice(&sig.s().to_be_bytes::<32>());
    bytes[64] = 27 + sig.v() as u8;
    format!("0x{}", hex::encode(bytes))
}

pub struct OrderSigner {
    signer: PrivateKeySigner,
    chain_id: u64,
    #[allow(dead_code)]
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

    /// Sign the EIP-712 `ClobAuth` struct (L1 auth) used to create/derive API
    /// credentials. `timestamp` is the unix-seconds string sent as POLY_TIMESTAMP.
    pub fn sign_clob_auth(&self, timestamp: &str, nonce: u64) -> Result<String> {
        let auth = ClobAuth {
            address: self.signer.address(),
            timestamp: timestamp.to_string(),
            nonce: U256::from(nonce),
            message: CLOB_AUTH_MESSAGE.to_string(),
        };
        let domain = eip712_domain! {
            name: "ClobAuthDomain",
            version: "1",
            chain_id: self.chain_id,
        };
        let hash: B256 = auth.eip712_signing_hash(&domain);
        let sig = self.signer.sign_hash_sync(&hash).context("signing ClobAuth")?;
        Ok(encode_signature(&sig))
    }
}
