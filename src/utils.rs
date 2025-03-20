use crate::error::{Result, WalletError};
use alloy::{network::EthereumWallet, signers::local::PrivateKeySigner};
use dotenv::dotenv;

pub const GAS_LIMIT: u64 = 21000;

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

pub async fn generate_wallet() -> Result<EthereumWallet> {
    let signer = PrivateKeySigner::random();
    let wallet = EthereumWallet::new(signer);
    Ok(wallet)
}

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
