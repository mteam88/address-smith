use std::{collections::HashSet, ops::Mul, sync::Arc, time::Duration};

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{ExecutionResult, FailureImpact, NodeError, NodeExecutionResult, Operation},
    utils::{get_eth_price, GAS_LIMIT},
};
use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{utils::format_units, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use alloy_primitives::utils::parse_units;
use futures::future;
use log::{info, warn};

/// Configuration for wallet operations loaded from environment variables
#[derive(Debug, Clone)]
pub struct Config {
    /// Multiplier for gas buffer to ensure sufficient gas is reserved
    pub gas_buffer_multiplier: u64,
    /// Maximum number of retry attempts for failed transactions
    pub max_retries: u32,
    /// Base delay in milliseconds between retry attempts
    pub retry_base_delay_ms: u64,
}

impl Config {
    /// Creates a new Config instance by loading values from environment variables.
    /// This should be called only once during startup.
    pub fn from_env() -> Result<Self> {
        dotenv::dotenv().ok();

        let gas_buffer_multiplier = dotenv::var("GAS_BUFFER_MULTIPLIER")
            .map_err(|_| WalletError::EnvVarNotFound("GAS_BUFFER_MULTIPLIER".to_string()))
            .and_then(|v| {
                v.parse::<u64>().map_err(|_| {
                    WalletError::InvalidEnvVar(
                        "GAS_BUFFER_MULTIPLIER must be a positive number".to_string(),
                    )
                })
            })?;

        let max_retries = dotenv::var("MAX_RETRIES")
            .unwrap_or_else(|_| "3".to_string())
            .parse()
            .unwrap_or(3);

        let retry_base_delay_ms = dotenv::var("RETRY_BASE_DELAY_MS")
            .unwrap_or_else(|_| "1000".to_string())
            .parse()
            .unwrap_or(1000);

        Ok(Self {
            gas_buffer_multiplier,
            max_retries,
            retry_base_delay_ms,
        })
    }
}

/// Manages wallet operations and transaction execution
pub struct WalletManager {
    provider: Arc<dyn Provider<Ethereum>>,
    /// Operations tree. Every sub-operation is dependent on the completion of it's parent operation.
    pub operations: Option<TreeNode<Operation>>,
    config: Config,
}

impl WalletManager {
    /// Creates a new WalletManager instance
    ///
    /// # Arguments
    /// * `provider` - Ethereum provider for blockchain interactions
    ///
    /// # Returns
    /// * `Result<Self>` - New WalletManager instance or error
    pub async fn new(provider: Arc<dyn Provider<Ethereum>>) -> Result<Self> {
        let config = Config::from_env()?;

        Ok(Self {
            provider: provider.clone(),
            operations: None,
            config,
        })
    }

    /// Executes operations in parallel, ensuring parent operations complete before children
    pub async fn parallel_execute_operations(&mut self) -> Result<ExecutionResult> {
        let start_time = tokio::time::Instant::now();

        let root_node = self.operations.as_ref().unwrap();
        let root_wallet = root_node.value.from.clone();
        let initial_balance = self.get_wallet_balance(&root_wallet).await?;
        let mut new_wallets = HashSet::new();
        let mut errors = Vec::new();

        // If the root node has multiple children, execute them in parallel
        // This is especially beneficial for the split_loops pattern where multiple loops run in parallel
        if !root_node.children.is_empty() {
            // Execute root operation first
            self.log(&format!("Executing root operation: {}", root_node.value));
            if let Err(e) = self
                .process_single_operation(&root_node.value, &mut new_wallets)
                .await
            {
                errors.push(NodeError {
                    node_id: root_node.id,
                    error: e,
                });
            }

            // Now execute all children in parallel
            let mut child_futures = Vec::with_capacity(root_node.children.len());

            for child in root_node.children.clone() {
                child_futures.push(self.execute_node(child));
            }

            // Wait for all child operations to complete
            let results = future::join_all(child_futures).await;

            // Process results
            for result in results {
                match result {
                    Ok(node_result) => {
                        new_wallets.extend(node_result.new_wallets);
                        errors.extend(node_result.errors);
                    }
                    Err(e) => {
                        // This should never happen as execute_node now returns NodeExecutionResult
                        errors.push(NodeError {
                            node_id: 0, // Unknown node ID in this case
                            error: e,
                        });
                    }
                }
            }
        } else {
            // If there's only a simple chain, just execute from the root
            let node_result = self.execute_node(root_node.clone()).await?;
            new_wallets.extend(node_result.new_wallets);
            errors.extend(node_result.errors);
        }

        let final_balance = self.get_wallet_balance(&root_wallet).await?;

        Ok(ExecutionResult {
            new_wallets_count: new_wallets.len() as i32,
            initial_balance,
            final_balance,
            root_wallet,
            time_elapsed: start_time.elapsed(),
            errors,
        })
    }

    async fn execute_node(&self, node: TreeNode<Operation>) -> Result<NodeExecutionResult> {
        let mut new_wallets = HashSet::new();
        let mut errors = Vec::new();

        // Execute operation
        let operation = node.value.clone();
        self.log(&format!("Executing operation: {}", operation));
        if let Err(e) = self
            .process_single_operation(&operation, &mut new_wallets)
            .await
        {
            errors.push(NodeError {
                node_id: node.id,
                error: e,
            });
        }

        // Now that parent operation is complete, handle children
        let children = node.children.clone();

        if !children.is_empty() {
            // Create a vector to store the futures
            let mut child_futures = Vec::with_capacity(children.len());

            // Create futures for all child operations without spawning new tasks
            for child in children {
                child_futures.push(self.execute_node(child));
            }

            // Execute all child operations concurrently and collect results
            let results = future::join_all(child_futures).await;

            // Process the results
            for result in results {
                match result {
                    Ok(child_result) => {
                        new_wallets.extend(child_result.new_wallets);
                        errors.extend(child_result.errors);
                    }
                    Err(e) => {
                        // This should never happen as execute_node now returns NodeExecutionResult
                        errors.push(NodeError {
                            node_id: 0, // Unknown node ID in this case
                            error: e,
                        });
                    }
                }
            }
        }

        Ok(NodeExecutionResult {
            new_wallets,
            errors,
        })
    }

    /// Gets the balance of a wallet
    async fn get_wallet_balance(&self, wallet: &EthereumWallet) -> Result<U256> {
        self.provider
            .get_balance(wallet.default_signer().address())
            .await
            .map_err(|e| WalletError::ProviderError(format!("Failed to get wallet balance: {}", e)))
    }

    /// Processes a single operation, including checking for new wallets
    async fn process_single_operation(
        &self,
        operation: &Operation,
        new_wallets: &mut HashSet<alloy::primitives::Address>,
    ) -> Result<()> {
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

        if operation.from.default_signer().address() == operation.to.default_signer().address() {
            return Ok(());
        }

        self.build_and_send_operation(operation).await
    }

    /// Logs a message
    fn log(&self, message: &str) {
        info!("{}", message);
    }

    /// Builds a transaction request with current network parameters
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

        let max_value = self.calculate_max_value(&from_wallet, gas_price).await?;
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

    /// Calculates the maximum value that can be sent in a transaction
    async fn calculate_max_value(&self, wallet: &EthereumWallet, gas_price: U256) -> Result<U256> {
        let balance = self.get_wallet_balance(wallet).await?;
        let gas = gas_price * U256::from(GAS_LIMIT);
        let gas_buffer = U256::from(self.config.gas_buffer_multiplier);
        Ok(balance - gas_buffer.mul(gas))
    }

    /// Sends a transaction without retry logic
    async fn send_transaction(&self, tx: TransactionRequest, wallet: EthereumWallet) -> Result<()> {
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
        self.log("Sending transaction...");

        let tx_envelope = tx.clone().build(wallet).await.map_err(|e| {
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
        self.log(&format!("Transaction Landed! Time elapsed: {:?}", duration));
        self.log(&format!("TX Hash: {}", hash));
        self.log(&format!(
            "TX Value: {}",
            format_units(tx.value.unwrap(), "ether").map_err(|e| {
                WalletError::TransactionError(format!("Failed to format transaction value: {}", e))
            })?
        ));
        Ok(())
    }

    /// Builds and sends an operation with retry logic, rebuilding transaction on each attempt
    async fn build_and_send_operation(&self, operation: &Operation) -> Result<()> {
        let mut retry_count = 0;
        let max_retries = self.config.max_retries;
        let base_delay = Duration::from_millis(self.config.retry_base_delay_ms);

        loop {
            // Rebuild transaction from scratch on each attempt
            let tx = self
                .build_transaction(
                    operation.from.clone(),
                    operation.to.default_signer().address(),
                    operation.amount,
                )
                .await?;

            match self.send_transaction(tx, operation.from.clone()).await {
                Ok(_) => return Ok(()),
                Err(e) => {
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
    }

    /// Prints execution statistics including time, addresses activated, and costs
    pub async fn print_statistics(&self, execution_result: ExecutionResult) -> Result<()> {
        let total_duration = execution_result.time_elapsed;
        let eth_spent = execution_result.initial_balance - execution_result.final_balance;
        let eth_price = get_eth_price().await?;

        self.log("\n========================================\nActivation Complete!\n========================================\n");

        // Print error summary if any errors occurred
        if !execution_result.errors.is_empty() {
            self.log(&format!(
                "\nErrors occurred during execution ({} total):",
                execution_result.errors.len()
            ));

            // Track total impact
            let mut total_eth_stuck = U256::ZERO;
            let mut total_orphaned_ops = 0;

            for error in &execution_result.errors {
                self.log(&format!("Node {}: {}", error.node_id, error.error));

                // Analyze and print the impact of this failure
                match self.analyze_failure_impact(error.node_id).await {
                    Ok(impact) => {
                        self.log(&format!("{}", impact));
                        total_eth_stuck += impact.eth_stuck;
                        total_orphaned_ops += impact.orphaned_operations;
                    }
                    Err(e) => {
                        self.log(&format!("Failed to analyze impact: {}", e));
                    }
                }
                self.log("\n");
            }

            // Print total impact statistics
            self.log("\nTotal Impact Summary:");
            self.log(&format!(
                "Total ETH Stuck: {} ETH (${:.2})",
                format_units(total_eth_stuck, "ether").map_err(|e| {
                    WalletError::TransactionError(format!("Failed to format ETH stuck: {}", e))
                })?,
                eth_price
                    * format_units(total_eth_stuck, "ether")
                        .map_err(|e| {
                            WalletError::TransactionError(format!(
                                "Failed to format ETH stuck: {}",
                                e
                            ))
                        })?
                        .parse::<f64>()
                        .unwrap()
            ));
            self.log(&format!(
                "Total Orphaned Operations: {}",
                total_orphaned_ops
            ));
        }

        self.log(&format!("Total Time Elapsed: {:?}", total_duration));
        self.log(&format!(
            "Total Addresses Activated: {}",
            execution_result.new_wallets_count
        ));
        self.log(&format!(
            "Average Time Per Address: {:?}",
            total_duration / execution_result.new_wallets_count as u32
        ));
        self.log(&format!(
            "Total ETH Spent: {} ETH (${:.2})",
            format_units(eth_spent, "ether").map_err(|e| {
                WalletError::TransactionError(format!("Failed to format ETH spent: {}", e))
            })?,
            eth_price
                * format_units(eth_spent, "ether")
                    .map_err(|e| {
                        WalletError::TransactionError(format!("Failed to format ETH spent: {}", e))
                    })?
                    .parse::<f64>()
                    .unwrap()
        ));

        let eth_per_wallet = parse_units(
            (format_units(eth_spent, "ether")
                .map_err(|e| {
                    WalletError::TransactionError(format!("Failed to format ETH spent: {}", e))
                })?
                .parse::<f64>()
                .unwrap()
                / execution_result.new_wallets_count as f64)
                .to_string()
                .as_str(),
            "ether",
        )
        .unwrap();
        // per wallet cost in eth and usd
        self.log(&format!(
            "Cost Per Wallet: {} ETH (${:.2})",
            format_units(eth_per_wallet, "ether").map_err(|e| {
                WalletError::TransactionError(format!("Failed to format ETH spent: {}", e))
            })?,
            eth_price
                * format_units(eth_per_wallet, "ether")
                    .map_err(|e| {
                        WalletError::TransactionError(format!("Failed to format ETH spent: {}", e))
                    })?
                    .parse::<f64>()
                    .unwrap()
        ));

        Ok(())
    }

    /// Analyzes the impact of a failed operation
    ///
    /// # Arguments
    /// * `node_id` - ID of the failed node
    ///
    /// # Returns
    /// * `Result<FailureImpact>` - Analysis of the failure's impact
    pub async fn analyze_failure_impact(&self, node_id: usize) -> Result<FailureImpact> {
        let root_node = self.operations.as_ref().ok_or_else(|| {
            WalletError::WalletOperationError("No operations tree available".to_string())
        })?;

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

    /// Recursively collects all node IDs that would be orphaned by a failure
    fn collect_orphaned_nodes(node: &TreeNode<Operation>, orphaned_ids: &mut Vec<usize>) {
        for child in &node.children {
            orphaned_ids.push(child.id);
            Self::collect_orphaned_nodes(child, orphaned_ids);
        }
    }
}
