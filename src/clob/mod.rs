//! Polymarket CLOB integration: EIP-712 signing, L2 auth, and the REST client.

mod auth;
mod client;
mod signing;

pub use auth::L2Creds;
pub use client::ClobClient;
pub use signing::{OrderInputs, OrderSigner};
