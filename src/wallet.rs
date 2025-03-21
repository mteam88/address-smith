use std::{collections::HashSet, io::Write, ops::Mul, path::PathBuf, sync::Arc, time::Duration};

use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{utils::format_units, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use futures::future;
use log::{info, warn};

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{ExecutionResult, NodeExecutionResult, Operation},
    utils::{get_eth_price, GAS_LIMIT},
};

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
    id: usize,
    provider: Arc<dyn Provider<Ethereum>>,
    /// Operations tree. Every sub-operation is dependent on the completion of it's parent operation.
    pub operations: Option<TreeNode<Operation>>,
    log_file: PathBuf,
    config: Config,
}

impl WalletManager {
    /// Creates a new WalletManager instance
    ///
    /// # Arguments
    /// * `id` - Unique identifier for this wallet manager
    /// * `provider` - Ethereum provider for blockchain interactions
    ///
    /// # Returns
    /// * `Result<Self>` - New WalletManager instance or error
    pub async fn new(id: usize, provider: Arc<dyn Provider<Ethereum>>) -> Result<Self> {
        let log_file = PathBuf::from(format!("wallet_manager_{}.log", id));
        let config = Config::from_env()?;

        Ok(Self {
            id,
            provider: provider.clone(),
            operations: None,
            log_file,
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

        // If the root node has multiple children, execute them in parallel
        // This is especially beneficial for the split_loops pattern where multiple loops run in parallel
        if !root_node.children.is_empty() {
            // Execute root operation first
            self.log(&format!("Executing root operation: {}", root_node.value))?;
            self.process_single_operation(&root_node.value, &mut new_wallets)
                .await?;

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
                    Ok(node_result) => new_wallets.extend(node_result.new_wallets),
                    Err(e) => return Err(e),
                }
            }
        } else {
            // If there's only a simple chain, just execute from the root
            let node_result = self.execute_node(root_node.clone()).await?;
            new_wallets.extend(node_result.new_wallets);
        }

        let final_balance = self.get_wallet_balance(&root_wallet).await?;

        Ok(ExecutionResult {
            new_wallets_count: new_wallets.len() as i32,
            initial_balance,
            final_balance,
            root_wallet,
            time_elapsed: start_time.elapsed(),
        })
    }

    async fn execute_node(&self, node: TreeNode<Operation>) -> Result<NodeExecutionResult> {
        // Execute operation
        let operation = node.value.clone();
        let mut new_wallets = HashSet::new();
        self.log(&format!("Executing operation: {}", operation))?;
        self.process_single_operation(&operation, &mut new_wallets)
            .await?;

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
                    Ok(child_result) => new_wallets.extend(child_result.new_wallets),
                    Err(e) => return Err(e),
                }
            }
        }

        Ok(NodeExecutionResult { new_wallets })
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

        self.build_and_send_operation(operation).await
    }

    /// Logs a message to both file and console
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

    /// Sends a transaction with retry logic
    async fn send_transaction(&self, tx: TransactionRequest, wallet: EthereumWallet) -> Result<()> {
        let mut retry_count = 0;
        let max_retries = self.config.max_retries;
        let base_delay = Duration::from_millis(self.config.retry_base_delay_ms);

        loop {
            match self.attempt_transaction(&tx, &wallet).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    if retry_count >= max_retries {
                        return Err(e);
                    }
                    retry_count += 1;
                    let delay = base_delay.mul_f32(1.5f32.powi(retry_count as i32));

                    warn!(
                        "Transaction failed (attempt {}/{}), retrying in {:?}: {}",
                        retry_count, max_retries, delay, e
                    );

                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Attempts to send a single transaction
    async fn attempt_transaction(
        &self,
        tx: &TransactionRequest,
        wallet: &EthereumWallet,
    ) -> Result<()> {
        self.log("Sending transaction...")?;

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
        self.log(&format!("Transaction Landed! Time elapsed: {:?}", duration))?;
        self.log(&format!("TX Hash: {}", hash))?;
        self.log(&format!(
            "TX Value: {}",
            format_units(tx.value.unwrap(), "ether").map_err(|e| {
                WalletError::TransactionError(format!("Failed to format transaction value: {}", e))
            })?
        ))?;
        Ok(())
    }

    /// Builds and sends an operation with retry logic
    async fn build_and_send_operation(&self, operation: &Operation) -> Result<()> {
        let tx = self
            .build_transaction(
                operation.from.clone(),
                operation.to.default_signer().address(),
                operation.amount,
            )
            .await?;

        self.send_transaction(tx, operation.from.clone()).await
    }

    /// Prints execution statistics including time, addresses activated, and costs
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
        ))?;

        Ok(())
    }
}
