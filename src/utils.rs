//! Utility functions for wallet and price operations.
//!
//! This module provides helper functions for:
//! - Gas calculations
//! - Wallet generation
//! - Price fetching

use crate::error::{Result, WalletError};
use alloy::{network::EthereumWallet, signers::local::PrivateKeySigner};
use dotenv::dotenv;

/// Standard gas limit for basic ETH transfer transactions
pub const GAS_LIMIT: u64 = 21000;

/// Gets the gas buffer multiplier from environment variables.
/// This multiplier is used to ensure sufficient gas is reserved for transactions.
///
/// # Returns
/// * `Result<u64>` - The gas buffer multiplier or an error if not properly configured
pub fn get_gas_buffer_multiplier() -> Result<u64> {
    dotenv().ok();
    dotenv::var("GAS_BUFFER_MULTIPLIER")
        .map_err(|_| WalletError::EnvVarNotFound("GAS_BUFFER_MULTIPLIER".to_string()))
        .and_then(|v| {
            v.parse::<u64>().map_err(|_| {
                WalletError::InvalidEnvVar(
                    "GAS_BUFFER_MULTIPLIER must be a positive number".to_string(),
                )
            })
        })
}

/// Generates a new Ethereum wallet with a random private key.
///
/// # Returns
/// * `Result<EthereumWallet>` - A new wallet instance or an error if generation fails
pub async fn generate_wallet() -> Result<EthereumWallet> {
    let signer = PrivateKeySigner::random();
    let wallet = EthereumWallet::new(signer);
    Ok(wallet)
}

/// Fetches the current ETH/USD price from the Coinbase API.
///
/// # Returns
/// * `Result<f64>` - The current ETH price in USD or an error if the fetch fails
pub async fn get_eth_price() -> Result<f64> {
    let response = reqwest::get("https://api.coinbase.com/v2/prices/ETH-USD/spot")
        .await
        .map_err(|e| WalletError::ProviderError(format!("Failed to fetch ETH price: {}", e)))?;

    let body = response
        .text()
        .await
        .map_err(|e| WalletError::ProviderError(format!("Failed to read response body: {}", e)))?;

    let parsed: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| WalletError::ProviderError(format!("Failed to parse response: {}", e)))?;

    parsed["data"]["amount"]
        .as_str()
        .ok_or_else(|| WalletError::ProviderError("Missing price data in response".to_string()))?
        .parse::<f64>()
        .map_err(|e| WalletError::ProviderError(format!("Failed to parse price value: {}", e)))
}
