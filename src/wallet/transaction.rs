use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{utils::format_units, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use log::{info, warn};
use std::{ops::Mul, sync::Arc, time::Duration};

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{FailureImpact, Operation},
    utils::GAS_LIMIT,
};

use super::Config;

/// Manages transaction building and sending operations
pub struct TransactionManager {
    provider: Arc<dyn Provider<Ethereum>>,
    config: Config,
}

impl TransactionManager {
    /// Creates a new TransactionManager
    pub fn new(provider: Arc<dyn Provider<Ethereum>>, config: Config) -> Self {
        Self { provider, config }
    }

    /// Gets the balance of a wallet
    pub async fn get_wallet_balance(&self, wallet: &EthereumWallet) -> Result<U256> {
        self.provider
            .get_balance(wallet.default_signer().address())
            .await
            .map_err(|e| WalletError::ProviderError(format!("Failed to get wallet balance: {}", e)))
    }

    /// Logs a message
    pub fn log(&self, message: &str) {
        info!("{}", message);
    }

    /// Builds a transaction request with current network parameters
    pub async fn build_transaction(
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

        // If a specific value is provided, use it, otherwise calculate the max value
        let value = if let Some(specified_value) = value {
            specified_value
        } else {
            let max_value = self.calculate_max_value(&from_wallet, gas_price).await?;
            // If max_value is zero, it likely means insufficient balance, which should error
            // only when trying to send all available funds
            if max_value.is_zero() {
                let balance = self.get_wallet_balance(&from_wallet).await?;
                let gas = gas_price * U256::from(GAS_LIMIT);
                let gas_buffer = U256::from(self.config.gas_buffer_multiplier);
                let total_gas_cost = gas_buffer.mul(gas);

                return Err(WalletError::InsufficientBalance(format!(
                    "Insufficient balance for gas buffer: {} < {} for wallet: {}.",
                    balance,
                    total_gas_cost,
                    from_wallet.default_signer().address()
                )));
            }
            max_value
        };

        Ok(TransactionRequest::default()
            .with_from(from_wallet.default_signer().address())
            .with_to(to_address)
            .with_value(value)
            .with_gas_limit(GAS_LIMIT)
            .with_gas_price(gas_price.to::<u128>())
            .with_nonce(nonce)
            .with_chain_id(chain_id))
    }

    /// Calculates the maximum value that can be sent in a transaction
    pub async fn calculate_max_value(
        &self,
        wallet: &EthereumWallet,
        gas_price: U256,
    ) -> Result<U256> {
        let balance = self.get_wallet_balance(wallet).await?;
        let gas = gas_price * U256::from(GAS_LIMIT);
        let gas_buffer = U256::from(self.config.gas_buffer_multiplier);
        let total_gas_cost = gas_buffer.mul(gas);

        if balance < total_gas_cost {
            warn!(
                "Insufficient balance for gas buffer: {} < {} for wallet: {}.",
                balance,
                total_gas_cost,
                wallet.default_signer().address()
            );
            // Return zero when balance is insufficient instead of throwing an error
            // The calling function will handle this appropriately
            return Ok(U256::ZERO);
        }

        Ok(balance - total_gas_cost)
    }

    /// Sends a transaction without retry logic
    pub async fn send_transaction(
        &self,
        tx: TransactionRequest,
        wallet: EthereumWallet,
    ) -> Result<()> {
        // slight random delay to avoid hitting rate limits
        let random_delay = Duration::from_millis(rand::random_range(0..2000));
        tokio::time::sleep(random_delay).await;

        self.attempt_transaction(&tx, &wallet).await
    }

    /// Attempts to send a single transaction
    async fn attempt_transaction(
        &self,
        tx: &TransactionRequest,
        wallet: &EthereumWallet,
    ) -> Result<()> {
        let tx_envelope = tx.clone().build(wallet).await.map_err(|e| {
            WalletError::TransactionError(format!("Failed to build transaction: {}", e), None)
        })?;

        // Safely calculate total gas cost without direct multiplication that could overflow
        let value_str = format_units(tx.value.unwrap(), "ether").map_err(|e| {
            WalletError::TransactionError(
                format!("Failed to format transaction value: {}", e),
                None,
            )
        })?;

        let gas_price_str = format!("{}", tx.gas_price.unwrap());

        self.log(&format!(
            "Sending transaction... Value: {} ETH, Gas Price: {} wei, Gas Limit: {}, Hash: {}",
            value_str,
            gas_price_str,
            GAS_LIMIT,
            tx_envelope.hash()
        ));

        let start = tokio::time::Instant::now();
        let receipt = self
            .provider
            .send_tx_envelope(tx_envelope)
            .await
            .map_err(|e| {
                WalletError::TransactionError(format!("Failed to send transaction: {}", e), None)
            })?
            .get_receipt()
            .await
            .map_err(|e| {
                WalletError::TransactionError(
                    format!("Failed to get transaction receipt: {}", e),
                    None,
                )
            })?;

        let duration = start.elapsed();

        self.log_transaction_success(tx, receipt.transaction_hash, duration)?;
        Ok(())
    }

    /// Logs successful transaction details
    fn log_transaction_success(
        &self,
        tx: &TransactionRequest,
        hash: alloy::primitives::TxHash,
        duration: Duration,
    ) -> Result<()> {
        self.log(&format!(
            "Transaction Landed! Time elapsed: {:?} | TX Hash: {} | TX Value: {}",
            duration,
            hash,
            format_units(tx.value.unwrap(), "ether").map_err(|e| WalletError::TransactionError(
                format!("Failed to format transaction value: {}", e),
                None
            ))?
        ));
        Ok(())
    }

    /// Builds and sends an operation with retry logic, rebuilding transaction on each attempt
    pub async fn build_and_send_operation(&self, operation: &Operation) -> Result<()> {
        let mut retry_count = 0;
        let max_retries = self.config.max_retries;
        let base_delay = Duration::from_millis(self.config.retry_base_delay_ms);

        loop {
            // Rebuild transaction from scratch on each attempt
            let tx_result = self
                .build_transaction(
                    operation.from.clone(),
                    operation.to.default_signer().address(),
                    operation.amount,
                )
                .await;

            // Handle InsufficientBalance error as a retriable error
            match tx_result {
                Ok(tx) => {
                    match self.send_transaction(tx, operation.from.clone()).await {
                        Ok(_) => return Ok(()),
                        Err(e) => {
                            // if e is transaction error, we must check if the transaction actually did land
                            if let WalletError::TransactionError(_, Some(hash)) = e {
                                // warn log
                                warn!(
                                    "Transaction failed, waiting 10 seconds before checking if it landed"
                                );
                                // let rpc think
                                tokio::time::sleep(Duration::from_secs(10)).await;
                                let receipt = self.provider.get_transaction_receipt(hash).await;
                                if let Ok(Some(receipt)) = receipt {
                                    if receipt.status() {
                                        return Ok(());
                                    }
                                }
                            }

                            if retry_count >= max_retries {
                                return Err(e);
                            }
                            retry_count += 1;
                            let delay = base_delay.mul_f32(1.5f32.powi(retry_count as i32));

                            warn!(
                                "Transaction failed (attempt {}/{}), rebuilding and retrying in {:?}: {}",
                                retry_count, max_retries, delay, e
                            );

                            tokio::time::sleep(delay).await;
                        }
                    }
                }
                Err(e) => {
                    // For InsufficientBalance, retry after waiting for more funds to arrive
                    if let WalletError::InsufficientBalance(msg) = &e {
                        if retry_count >= max_retries {
                            return Err(e);
                        }
                        retry_count += 1;
                        let delay = base_delay.mul_f32(1.5f32.powi(retry_count as i32));

                        warn!(
                            "Insufficient balance (attempt {}/{}), retrying in {:?}: {}",
                            retry_count, max_retries, delay, msg
                        );

                        tokio::time::sleep(delay).await;
                    } else {
                        // For other build errors, propagate them
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Analyzes the impact of a failed operation
    ///
    /// # Arguments
    /// * `node_id` - ID of the failed node
    /// * `root_node` - Reference to the root operation node
    ///
    /// # Returns
    /// * `Result<FailureImpact>` - Analysis of the failure's impact
    pub async fn analyze_failure_impact(
        &self,
        node_id: usize,
        root_node: &TreeNode<Operation>,
    ) -> Result<FailureImpact> {
        // Find the failed node
        let failed_node = root_node.find_node_by_id(node_id).ok_or_else(|| {
            WalletError::WalletOperationError(format!("Node with ID {} not found", node_id))
        })?;

        // Get stuck ETH amount
        let eth_stuck = self
            .get_wallet_balance(&failed_node.value.from)
            .await
            .unwrap_or(U256::ZERO);
        let stuck_address = failed_node.value.from.default_signer().address();

        // Collect all orphaned nodes
        let mut orphaned_node_ids = Vec::new();
        Self::collect_orphaned_nodes(failed_node, &mut orphaned_node_ids);

        Ok(FailureImpact {
            failed_node_id: node_id,
            eth_stuck,
            stuck_address,
            orphaned_operations: orphaned_node_ids.len(),
            orphaned_node_ids,
        })
    }

    /// Collects all node IDs that would be orphaned by a failure using an iterative approach
    fn collect_orphaned_nodes(node: &TreeNode<Operation>, orphaned_ids: &mut Vec<usize>) {
        let mut stack = vec![node];

        while let Some(current) = stack.pop() {
            // Add all children to the result and to the stack for processing
            for child in &current.children {
                orphaned_ids.push(child.id);
                stack.push(child);
            }
        }
    }
}
