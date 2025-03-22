use std::{collections::HashSet, io::Write, ops::Mul, sync::Arc, time::Duration};

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{
        ExecutionResult, FailureImpact, NodeError, NodeExecutionResult, Operation, ProgressStats,
    },
    utils::{get_eth_price, GAS_LIMIT},
};
use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{utils::format_units, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};
use alloy_primitives::utils::parse_units;
use log::{info, warn};
use tokio::sync::RwLock;

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
    /// Progress statistics for the current execution
    progress: Arc<RwLock<ProgressStats>>,
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
            progress: Arc::new(RwLock::new(ProgressStats::new(0))), // Will be initialized in parallel_execute_operations
        })
    }

    /// Updates progress statistics and prints current status
    async fn update_progress(&self, success: bool) {
        let mut progress = self.progress.write().await;
        progress.completed_operations += 1;
        if success {
            progress.successful_operations += 1;
        }

        // Update gas price
        if let Ok(gas_price) = self.provider.get_gas_price().await {
            progress.current_gas_price = U256::from(gas_price);
        }

        // Calculate statistics
        let success_rate = progress.success_rate();
        let progress_percent =
            (progress.completed_operations as f64 / progress.total_operations as f64) * 100.0;
        let ops_per_minute = progress.operations_per_minute();

        let time_remaining = progress
            .estimated_time_remaining()
            .map(|d| format!("{:.1} minutes", d.as_secs_f64() / 60.0))
            .unwrap_or_else(|| "calculating...".to_string());

        let gas_price_gwei =
            format_units(progress.current_gas_price, "gwei").unwrap_or_else(|_| "N/A".to_string());

        // Format all lines first to determine maximum width
        let lines = vec![
            format!(
                "Progress: {:.1}% ({}/{})",
                progress_percent, progress.completed_operations, progress.total_operations
            ),
            format!("Success Rate: {:.1}%", success_rate),
            format!("Gas Price: {} gwei", gas_price_gwei),
            format!("Operations/min: {:.1}", ops_per_minute),
            format!("Time Remaining: {}", time_remaining),
        ];

        // Calculate required width (add 6 for margins: 2 for borders + 2 spaces on each side)
        let max_width = lines.iter().map(|line| line.len()).max().unwrap_or(0) + 6;

        let title = "Operation Progress";
        let title_total_padding = max_width - 2 - title.len(); // -2 for the border characters
        let title_left_padding = title_total_padding / 2;
        let title_right_padding = title_total_padding - title_left_padding;
        let border_line = "═".repeat(max_width - 2);

        // Clear screen and print box
        print!("\x1B[2J\x1B[1;1H"); // Clear screen and move cursor to top
        println!("╔{}╗", border_line);
        println!(
            "║{}{}{}║",
            " ".repeat(title_left_padding),
            title,
            " ".repeat(title_right_padding)
        );
        println!("╠{}╣", border_line);

        // Print each line with proper padding
        for line in lines {
            let padding = max_width - line.len() - 4; // -4 for borders and minimum spaces
            println!("║  {}{}║", line, " ".repeat(padding));
        }

        println!("╚{}╝", border_line);

        // Ensure output is flushed
        std::io::stdout().flush().unwrap_or_default();
    }

    /// Executes operations in parallel, ensuring parent operations complete before children
    pub async fn parallel_execute_operations(&mut self) -> Result<ExecutionResult> {
        let start_time = tokio::time::Instant::now();

        let root_node = self.operations.as_ref().unwrap();

        // Count total operations for progress tracking
        let total_operations = Self::count_total_operations(root_node);
        *self.progress.write().await = ProgressStats::new(total_operations);

        let root_wallet = root_node.value.from.clone();
        let initial_balance = self.get_wallet_balance(&root_wallet).await?;

        // Execute all operations in the tree using the task queue approach
        let node_result = self
            .execute_operations_with_task_queue(root_node.clone())
            .await?;

        let final_balance = self.get_wallet_balance(&root_wallet).await?;

        Ok(ExecutionResult {
            new_wallets_count: node_result.new_wallets.len() as i32,
            initial_balance,
            final_balance,
            root_wallet,
            time_elapsed: start_time.elapsed(),
            errors: node_result.errors,
        })
    }

    /// Counts total number of operations in the tree using an iterative approach
    fn count_total_operations(node: &TreeNode<Operation>) -> usize {
        let mut count = 0;
        let mut stack = vec![node];

        while let Some(current) = stack.pop() {
            // Count the current node
            count += 1;

            // Add all children to the stack
            for child in &current.children {
                stack.push(child);
            }
        }

        count
    }

    /// Executes operations using a task queue approach to avoid recursion stack overflow
    /// while maintaining parallel execution of sibling operations
    async fn execute_operations_with_task_queue(
        &self,
        root_node: TreeNode<Operation>,
    ) -> Result<NodeExecutionResult> {
        // Create a task queue using VecDeque
        use futures::future;
        use std::collections::{HashMap, HashSet, VecDeque};

        // Store tasks and their state
        #[derive(Clone)]
        struct Task {
            node: TreeNode<Operation>,
            parent_id: Option<usize>, // None for root
            is_executed: bool,
        }

        let mut queue = VecDeque::new();
        let mut new_wallets = HashSet::new();
        let mut errors = Vec::new();
        let mut task_map: HashMap<usize, Task> = HashMap::new();

        // Add root task to the queue
        queue.push_back(Task {
            node: root_node.clone(),
            parent_id: None,
            is_executed: false,
        });

        // Process tasks until the queue is empty
        while !queue.is_empty() {
            // Identify all tasks that are ready to execute in parallel
            let mut ready_tasks = Vec::new();
            let mut pending_tasks = VecDeque::new();

            // Sort tasks into ready and pending
            while let Some(task) = queue.pop_front() {
                // Check if the task is ready to execute (root or parent has been executed)
                let parent_executed = task
                    .parent_id
                    .map(|id| task_map.get(&id).map(|t| t.is_executed).unwrap_or(false))
                    .unwrap_or(true); // Root has no parent, so it's ready

                if !task.is_executed && parent_executed {
                    // This task is ready to be executed
                    ready_tasks.push(task);
                } else {
                    // This task must wait - add it back to the pending queue
                    pending_tasks.push_back(task);
                }
            }

            // If we found no ready tasks but still have pending tasks, we have a dependency cycle
            if ready_tasks.is_empty() && !pending_tasks.is_empty() {
                return Err(WalletError::WalletOperationError(
                    "Dependency cycle detected in operations".to_string(),
                ));
            }

            // Process all ready tasks in parallel
            if !ready_tasks.is_empty() {
                // Create a future for each ready task
                let futures = ready_tasks
                    .iter()
                    .map(|task| {
                        let node = task.node.clone();
                        let mut task_new_wallets = HashSet::new();

                        async move {
                            let operation = node.value.clone();

                            // Execute the operation
                            let result = self
                                .process_single_operation(&operation, &mut task_new_wallets)
                                .await;

                            // Return a tuple of (node, result, task_new_wallets) to process after all parallel tasks complete
                            (node, result, task_new_wallets, task.clone())
                        }
                    })
                    .collect::<Vec<_>>();

                // Execute all ready tasks in parallel
                let results = future::join_all(futures).await;

                // Process results and mark tasks as completed
                for (node, result, task_new_wallets, mut task) in results {
                    let success = result.is_ok();

                    // Log the operation
                    self.log(&format!("Executing operation: {}", node.value));

                    // Update progress stats
                    self.update_progress(success).await;

                    // Collect new wallets
                    new_wallets.extend(task_new_wallets);

                    // Handle errors
                    if let Err(e) = result {
                        errors.push(NodeError {
                            node_id: node.id,
                            error: e,
                        });
                    }

                    // Mark task as executed
                    task.is_executed = true;
                    task_map.insert(node.id, task);

                    // If operation succeeded, queue up its children
                    if success {
                        for child in &node.children {
                            pending_tasks.push_back(Task {
                                node: child.clone(),
                                parent_id: Some(node.id),
                                is_executed: false,
                            });
                        }
                    }
                }
            }

            // Move pending tasks back to the main queue
            queue = pending_tasks;
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
    async fn calculate_max_value(&self, wallet: &EthereumWallet, gas_price: U256) -> Result<U256> {
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
    async fn build_and_send_operation(&self, operation: &Operation) -> Result<()> {
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
                    WalletError::TransactionError(
                        format!("Failed to format ETH stuck: {}", e),
                        None,
                    )
                })?,
                eth_price
                    * format_units(total_eth_stuck, "ether")
                        .map_err(|e| {
                            WalletError::TransactionError(
                                format!("Failed to format ETH stuck: {}", e),
                                None,
                            )
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
                WalletError::TransactionError(format!("Failed to format ETH spent: {}", e), None)
            })?,
            eth_price
                * format_units(eth_spent, "ether")
                    .map_err(|e| {
                        WalletError::TransactionError(
                            format!("Failed to format ETH spent: {}", e),
                            None,
                        )
                    })?
                    .parse::<f64>()
                    .unwrap()
        ));

        let eth_per_wallet = parse_units(
            (format_units(eth_spent, "ether")
                .map_err(|e| {
                    WalletError::TransactionError(
                        format!("Failed to format ETH spent: {}", e),
                        None,
                    )
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
                WalletError::TransactionError(format!("Failed to format ETH spent: {}", e), None)
            })?,
            eth_price
                * format_units(eth_per_wallet, "ether")
                    .map_err(|e| {
                        WalletError::TransactionError(
                            format!("Failed to format ETH spent: {}", e),
                            None,
                        )
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
