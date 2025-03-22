//! Utility functions for wallet and price operations.
//!
//! This module provides helper functions for:
//! - Gas calculations
//! - Wallet generation
//! - Price fetching

use std::path::Path;

use crate::{
    error::{Result, WalletError},
    Operation, TreeNode,
};
use alloy::{network::EthereumWallet, signers::local::PrivateKeySigner};
use dotenv::dotenv;
use log::info;

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
pub async fn generate_wallet(backup_dir: &Path) -> Result<EthereumWallet> {
    let signer = PrivateKeySigner::random();

    // backup private key to file
    let backup_path = backup_dir.join(format!("backup_wallet_{}.txt", signer.address()));
    std::fs::write(backup_path, signer.to_bytes().to_string())
        .map_err(|e| WalletError::ProviderError(format!("Failed to backup private key: {}", e)))?;

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

/// Pretty prints a tree of operations, showing the hierarchy with indentation.
/// Each operation shows the transfer details between wallets.
///
/// # Arguments
/// * `tree` - The root node of the operation tree to print
pub fn pretty_print_tree(tree: &TreeNode<Operation>) {
    // Stack holds (node, depth) pairs
    let mut stack = vec![(tree, 0)];

    // Process stack until empty
    while let Some((node, depth)) = stack.pop() {
        // Print the current node
        let indent = " | ".repeat(depth);
        info!("{}Node {}: Operation: {}", indent, node.id, node.value);

        // Add children to the stack in reverse order (so they print in correct order)
        for child in node.children.iter().rev() {
            stack.push((child, depth + 1));
        }
    }
}
