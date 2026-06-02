//! Polymarket CLOB integration: L1 (ClobAuth) signing used to derive/create the
//! API credentials. Live order signing + submission is handled by the official
//! `polymarket_client_sdk_v2` in the executor.

mod keys;
mod signing;

pub use keys::{create_or_derive_api_creds, DerivedCreds};
pub use signing::OrderSigner;
