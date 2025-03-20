use std::{
    io::Write,
    ops::Mul,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{map::HashSet, utils::format_units, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use log::info;

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{ExecutionResult, Operation},
    utils::{get_eth_price, get_gas_buffer_multiplier, GAS_LIMIT},
};

pub struct WalletManager {
    id: usize,
    provider: Arc<dyn Provider<Ethereum>>,
    /// Operations tree. Every sub-operation is dependent on the completion of it's parent operation.
    pub operations: Option<Arc<Mutex<TreeNode<Operation>>>>,
    log_file: PathBuf,
}

impl WalletManager {
    pub async fn new(id: usize, provider: Arc<dyn Provider<Ethereum>>) -> Result<Self> {
        let log_file = PathBuf::from(format!("wallet_manager_{}.log", id));

        Ok(Self {
            id,
            provider: provider.clone(),
            operations: None,
            log_file,
        })
    }

    pub async fn sequential_execute_operations(&mut self) -> Result<ExecutionResult> {
        let start_time = tokio::time::Instant::now();

        let operations = self.operations.as_ref().ok_or_else(|| {
            WalletError::WalletOperationError("No operations configured".to_string())
        })?;
        let operations_list = {
            let operations = operations.lock().map_err(|_| {
                WalletError::WalletOperationError("Failed to lock operations mutex".to_string())
            })?;
            operations.flatten()
        };

        let root_wallet = operations_list
            .first()
            .ok_or_else(|| {
                WalletError::WalletOperationError("No operations to execute".to_string())
            })?
            .from
            .clone();

        // if root_wallet.default_signer().address()
        //     != operations_list
        //         .last()
        //         .unwrap()
        //         .to
        //         .default_signer()
        //         .address()
        // {
        //     return Err(WalletError::WalletOperationError(
        //         "Root wallet address does not match last operation's to address".to_string(),
        //     ));
        // }

        let initial_balance = self
            .provider
            .get_balance(root_wallet.default_signer().address())
            .await
            .map_err(|e| {
                WalletError::ProviderError(format!("Failed to get initial balance: {}", e))
            })?;

        let mut new_wallets = HashSet::new();

        for operation in operations_list {
            self.log(&format!("Executing operation: {}", operation))?;

            let tx_count = self
                .provider
                .get_transaction_count(operation.from.default_signer().address())
                .await
                .map_err(|e| {
                    WalletError::ProviderError(format!("Failed to get transaction count: {}", e))
                })?;

            if tx_count == 0 {
                new_wallets.insert(operation.from.default_signer().address());
            }

            self.build_and_send_operation(operation).await?;
        }

        let new_wallets_count = new_wallets.len() as i32;
        let final_balance = self
            .provider
            .get_balance(root_wallet.default_signer().address())
            .await
            .map_err(|e| {
                WalletError::ProviderError(format!("Failed to get final balance: {}", e))
            })?;

        Ok(ExecutionResult {
            new_wallets_count,
            initial_balance,
            final_balance,
            root_wallet,
            time_elapsed: start_time.elapsed(),
        })
    }

    fn log(&self, message: &str) -> Result<()> {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let log_message = format!("[{}] {}\n", timestamp, message);

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)?;

        file.write_all(log_message.as_bytes())?;
        info!("[Manager {}] {}", self.id, message);
        Ok(())
    }

    async fn build_transaction(
        &self,
        from_wallet: EthereumWallet,
        to_address: alloy::primitives::Address,
        value: Option<U256>,
    ) -> Result<TransactionRequest> {
        let gas_price =
            U256::from(self.provider.get_gas_price().await.map_err(|e| {
                WalletError::ProviderError(format!("Failed to get gas price: {}", e))
            })?);

        let nonce = self
            .provider
            .get_transaction_count(from_wallet.default_signer().address())
            .await
            .map_err(|e| WalletError::ProviderError(format!("Failed to get nonce: {}", e)))?;

        let chain_id =
            self.provider.get_chain_id().await.map_err(|e| {
                WalletError::ProviderError(format!("Failed to get chain ID: {}", e))
            })?;

        let max_value = U256::from(
            self.provider
                .get_balance(from_wallet.default_signer().address())
                .await
                .map_err(|e| WalletError::ProviderError(format!("Failed to get balance: {}", e)))?,
        );

        let gas = gas_price * U256::from(GAS_LIMIT);
        let gas_buffer = get_gas_buffer_multiplier()?;
        let max_value = max_value - U256::from(gas_buffer).mul(gas);

        let value = value.unwrap_or(max_value);

        Ok(TransactionRequest::default()
            .with_from(from_wallet.default_signer().address())
            .with_to(to_address)
            .with_value(value)
            .with_gas_limit(GAS_LIMIT)
            .with_gas_price(gas_price.to::<u128>())
            .with_nonce(nonce)
            .with_chain_id(chain_id))
    }

    async fn send_transaction(&self, tx: TransactionRequest, wallet: EthereumWallet) -> Result<()> {
        self.log("Sending transaction...")?;
        let tx_envelope = tx.clone().build(&wallet).await.map_err(|e| {
            WalletError::TransactionError(format!("Failed to build transaction: {}", e))
        })?;

        let start = tokio::time::Instant::now();
        let receipt = self
            .provider
            .send_tx_envelope(tx_envelope)
            .await
            .map_err(|e| {
                WalletError::TransactionError(format!("Failed to send transaction: {}", e))
            })?
            .get_receipt()
            .await
            .map_err(|e| {
                WalletError::TransactionError(format!("Failed to get transaction receipt: {}", e))
            })?;

        let duration = start.elapsed();

        self.log(&format!("Transaction Landed! Time elapsed: {:?}", duration))?;
        self.log(&format!("TX Hash: {}", receipt.transaction_hash))?;
        self.log(&format!(
            "TX Value: {}",
            format_units(tx.value.unwrap(), "ether").map_err(|e| WalletError::TransactionError(
                format!("Failed to format transaction value: {}", e)
            ))?
        ))?;
        Ok(())
    }

    async fn build_and_send_operation(&self, operation: Operation) -> Result<()> {
        let tx = self
            .build_transaction(
                operation.from.clone(),
                operation.to.default_signer().address(),
                operation.amount,
            )
            .await?;

        self.send_transaction(tx, operation.from.clone()).await?;
        Ok(())
    }

    pub async fn print_statistics(&self, execution_result: ExecutionResult) -> Result<()> {
        let total_duration = execution_result.time_elapsed;
        let eth_spent = execution_result.initial_balance - execution_result.final_balance;
        let eth_price = get_eth_price().await?;

        self.log("\n========================================\nActivation Complete!\n========================================\n")?;

        self.log(&format!("Total Time Elapsed: {:?}", total_duration))?;
        self.log(&format!(
            "Total Addresses Activated: {}",
            execution_result.new_wallets_count
        ))?;
        self.log(&format!(
            "Average Time Per Address: {:?}",
            total_duration / execution_result.new_wallets_count as u32
        ))?;
        self.log(&format!(
            "Total ETH Cost: {}",
            format_units(eth_spent.to::<i128>(), "ether").map_err(|e| {
                WalletError::TransactionError(format!("Failed to format ETH cost: {}", e))
            })?
        ))?;
        self.log(&format!(
            "Average ETH Cost Per Address: {}",
            format_units(
                eth_spent.to::<i128>() / execution_result.new_wallets_count as i128,
                "ether"
            )
            .map_err(|e| WalletError::TransactionError(format!(
                "Failed to format average ETH cost: {}",
                e
            )))?
        ))?;
        self.log(&format!(
            "Total USD Cost: {}",
            format_units(eth_spent.to::<i128>(), "ether")
                .map_err(|e| WalletError::TransactionError(format!(
                    "Failed to format USD cost: {}",
                    e
                )))?
                .parse::<f64>()
                .map_err(|e| WalletError::TransactionError(format!(
                    "Failed to parse USD cost: {}",
                    e
                )))?
                * eth_price
        ))?;
        self.log(&format!(
            "Average USD Cost Per Address: {}",
            format_units(eth_spent.to::<i128>(), "ether")
                .map_err(|e| WalletError::TransactionError(format!(
                    "Failed to format average USD cost: {}",
                    e
                )))?
                .parse::<f64>()
                .map_err(|e| WalletError::TransactionError(format!(
                    "Failed to parse average USD cost: {}",
                    e
                )))?
                * eth_price
                / execution_result.new_wallets_count as f64
        ))?;
        self.log(&format!(
            "Final Balance: {} has been sent back to the original wallet: {}",
            format_units(execution_result.final_balance, "ether").map_err(|e| {
                WalletError::TransactionError(format!("Failed to format final balance: {}", e))
            })?,
            execution_result.root_wallet.default_signer().address()
        ))?;
        Ok(())
    }
}
