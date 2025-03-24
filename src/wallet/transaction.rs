use alloy::{
    network::{Ethereum, TransactionBuilder},
    primitives::{utils::format_units, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use alloy_primitives::Address;
use std::{ops::Mul, sync::Arc, time::Duration};
use tracing::{info, info_span, warn};

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
    pub async fn get_wallet_balance(&self, address: &Address) -> Result<U256> {
        let balance_span = info_span!(
            "get_balance",
            address = %address
        );
        let _guard = balance_span.enter();

        let balance = self.provider.get_balance(*address).await.map_err(|e| {
            WalletError::ProviderError(format!("Failed to get wallet balance: {}", e))
        })?;

        info!(balance = ?format_units(balance, "ether").unwrap_or_default());
        Ok(balance)
    }

    /// Builds a transaction request with current network parameters
    pub async fn build_transaction(
        &self,
        from_address: Address,
        to_address: Address,
        value: Option<U256>,
    ) -> Result<TransactionRequest> {
        let build_span = info_span!(
            "build_transaction",
            from = %from_address,
            to = %to_address,
            value = ?value.map(|v| format_units(v, "ether").unwrap_or_default()),
        );
        let _guard = build_span.enter();

        let gas_price =
            U256::from(self.provider.get_gas_price().await.map_err(|e| {
                WalletError::ProviderError(format!("Failed to get gas price: {}", e))
            })?);

        // If a specific value is provided, use it, otherwise calculate the max value
        let value = if let Some(specified_value) = value {
            specified_value
        } else {
            let max_value = self.calculate_max_value(&from_address, gas_price).await?;
            // If max_value is zero, it likely means insufficient balance, which should error
            // only when trying to send all available funds
            if max_value.is_zero() {
                let balance = self.get_wallet_balance(&from_address).await?;
                let gas = gas_price * U256::from(GAS_LIMIT);
                let gas_buffer = U256::from(self.config.gas_buffer_multiplier);
                let total_gas_cost = gas_buffer.mul(gas);

                return Err(WalletError::InsufficientBalance(format!(
                    "Insufficient balance for gas buffer: {} < {} for wallet: {}.",
                    balance, total_gas_cost, from_address
                )));
            }
            max_value
        };

        info!(
            gas_price = ?format_units(gas_price, "gwei").unwrap_or_default(),
            final_value = ?format_units(value, "ether").unwrap_or_default(),
            "Transaction parameters prepared"
        );

        Ok(TransactionRequest::default()
            .with_from(from_address)
            .with_to(to_address)
            .with_value(value)
            .with_gas_limit(GAS_LIMIT)
            .with_gas_price(gas_price.to::<u128>()))
    }

    /// Calculates the maximum value that can be sent in a transaction
    pub async fn calculate_max_value(&self, address: &Address, gas_price: U256) -> Result<U256> {
        let calc_span = info_span!(
            "calculate_max_value",
            address = %address,
            gas_price = ?format_units(gas_price, "gwei").unwrap_or_default(),
        );
        let _guard = calc_span.enter();

        let balance = self.get_wallet_balance(address).await?;
        let gas = gas_price * U256::from(GAS_LIMIT);
        let gas_buffer = U256::from(self.config.gas_buffer_multiplier);
        let total_gas_cost = gas_buffer.mul(gas);

        if balance < total_gas_cost {
            warn!(
                balance = ?format_units(balance, "ether").unwrap_or_default(),
                required = ?format_units(total_gas_cost, "ether").unwrap_or_default(),
                address = %address,
                "Insufficient balance for gas buffer"
            );
            return Ok(U256::ZERO);
        }

        let max_value = balance - total_gas_cost;
        info!(
            max_value = ?format_units(max_value, "ether").unwrap_or_default(),
            "Maximum sendable value calculated"
        );
        Ok(max_value)
    }

    /// Sends a transaction without retry logic
    pub async fn send_transaction(&self, tx: TransactionRequest) -> Result<()> {
        let send_span = info_span!(
            "send_transaction",
            from = %tx.from.unwrap().to_string(),
            value = ?format_units(tx.value.unwrap(), "ether").unwrap_or_default(),
            // nonce = tx.nonce.unwrap(),
        );
        let _guard = send_span.enter();

        // slight random delay to avoid hitting rate limits
        let random_delay = Duration::from_millis(rand::random_range(0..2000));
        tokio::time::sleep(random_delay).await;

        self.attempt_transaction(&tx).await
    }

    /// Attempts to send a single transaction
    async fn attempt_transaction(&self, tx: &TransactionRequest) -> Result<()> {
        let start = tokio::time::Instant::now();
        let attempt_span = info_span!(
            "attempt_transaction",
            from = %tx.from.unwrap().to_string(),
            value = ?format_units(tx.value.unwrap(), "ether").unwrap_or_default(),
            // nonce = tx.nonce.unwrap(),
        );
        let _guard = attempt_span.enter();

        let pending = self
            .provider
            .send_transaction(tx.clone())
            .await
            .map_err(|e| {
                WalletError::TransactionError(format!("Failed to send transaction: {}", e), None)
            })?;

        let tx_hash = *pending.tx_hash();

        let receipt = pending.get_receipt().await.map_err(|e| {
            WalletError::TransactionError(
                format!("Failed to get transaction receipt: {}", e),
                Some(tx_hash),
            )
        })?;

        let duration = start.elapsed();

        info!(
            tx_hash = %tx_hash,
            duration = ?duration,
            status = ?receipt.status(),
            "Transaction confirmed"
        );

        Ok(())
    }

    /// Builds and sends an operation with retry logic, rebuilding transaction on each attempt.
    ///
    /// This function implements a robust retry mechanism for transaction sending:
    /// 1. Builds a new transaction with current network parameters
    /// 2. Attempts to send the transaction
    /// 3. On failure, implements exponential backoff and retries up to max_retries
    /// 4. Verifies transaction receipt even if initial send fails
    /// 5. Handles special cases like insufficient balance and transaction errors
    ///
    /// # Arguments
    /// * `operation` - The operation to execute
    ///
    /// # Returns
    /// * `Result<()>` - Success or error with details
    ///
    /// # Errors
    /// * `WalletError::InsufficientBalance` - When wallet has insufficient funds
    /// * `WalletError::TransactionError` - When transaction fails after all retries
    /// * `WalletError::ProviderError` - When RPC communication fails
    pub async fn build_and_send_operation(&self, operation: &Operation) -> Result<()> {
        let op_span = info_span!(
            "operation",
            from = %operation.from,
            to = %operation.to,
            amount = ?operation.amount.map(|a| format_units(a, "ether").unwrap_or_default()),
        );
        let _guard = op_span.enter();

        let mut retry_count = 0;
        let max_retries = self.config.max_retries;
        let base_delay = Duration::from_millis(self.config.retry_base_delay_ms);

        loop {
            let attempt_span = info_span!("attempt", retry_count);
            let _attempt_guard = attempt_span.enter();

            // Rebuild transaction from scratch on each attempt
            let tx_result = self
                .build_transaction(operation.from, operation.to, operation.amount)
                .await;

            // Handle InsufficientBalance error as a retriable error
            match tx_result {
                Ok(tx) => {
                    match self.send_transaction(tx).await {
                        Ok(_) => return Ok(()),
                        Err(e) => {
                            // if e is transaction error , we must check if the transaction actually did land
                            if let WalletError::TransactionError(_, Some(hash)) = e {
                                warn!(
                                    tx_hash = %hash,
                                    "Transaction failed, checking if it landed"
                                );
                                // let rpc think
                                tokio::time::sleep(Duration::from_secs(10)).await;
                                let receipt = self.provider.get_transaction_receipt(hash).await;
                                if let Ok(Some(receipt)) = receipt {
                                    if receipt.status() {
                                        info!(
                                            tx_hash = %hash,
                                            "Transaction succeeded despite error"
                                        );
                                        return Ok(());
                                    }
                                }
                            }

                            if retry_count >= max_retries {
                                warn!(
                                    error = %e,
                                    retry_count,
                                    max_retries,
                                    "Max retries reached"
                                );
                                return Err(e);
                            }
                            retry_count += 1;
                            let delay = base_delay.mul_f32(1.5f32.powi(retry_count as i32));

                            warn!(
                                error = %e,
                                retry_count,
                                max_retries,
                                ?delay,
                                "Retrying transaction"
                            );

                            tokio::time::sleep(delay).await;
                        }
                    }
                }
                Err(e) => {
                    if let WalletError::InsufficientBalance(msg) = &e {
                        if retry_count >= max_retries {
                            warn!(
                                error = %e,
                                retry_count,
                                max_retries,
                                "Max retries reached"
                            );
                            return Err(e);
                        }
                        retry_count += 1;
                        let delay = base_delay.mul_f32(1.5f32.powi(retry_count as i32));

                        warn!(
                            error = %msg,
                            retry_count,
                            max_retries,
                            ?delay,
                            "Insufficient balance, retrying"
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
    pub async fn analyze_failure_impact(
        &self,
        node_id: usize,
        root_node: &TreeNode<Operation>,
    ) -> Result<FailureImpact> {
        let analyze_span = info_span!("analyze_failure", node_id);
        let _guard = analyze_span.enter();

        // Find the failed node
        let failed_node = root_node.find_node_by_id(node_id).ok_or_else(|| {
            WalletError::WalletOperationError(format!("Node with ID {} not found", node_id))
        })?;

        // Get stuck ETH amount
        let eth_stuck = self
            .get_wallet_balance(&failed_node.value.from)
            .await
            .unwrap_or(U256::ZERO);
        let stuck_address = failed_node.value.from;

        // Collect all orphaned nodes
        let mut orphaned_node_ids = Vec::new();
        Self::collect_orphaned_nodes(failed_node, &mut orphaned_node_ids);

        info!(
            eth_stuck = ?format_units(eth_stuck, "ether").unwrap_or_default(),
            stuck_address = %stuck_address,
            orphaned_operations = orphaned_node_ids.len(),
            "Failure impact analyzed"
        );

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
            for child in &current.children {
                orphaned_ids.push(child.id);
                stack.push(child);
            }
        }
    }
}
