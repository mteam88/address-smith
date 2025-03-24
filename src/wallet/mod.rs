pub mod execution;
pub mod progress;
pub mod transaction;

use alloy::{
    network::Ethereum,
    primitives::{Address, U256},
    providers::Provider,
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{ExecutionResult, FailureImpact, Operation, ProgressStats},
};

use self::{
    execution::ExecutionManager, progress::ProgressManager, transaction::TransactionManager,
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
    pub provider: Arc<dyn Provider<Ethereum>>,
    /// Operations tree. Every sub-operation is dependent on the completion of it's parent operation.
    pub operations: Option<TreeNode<Operation>>,
    pub config: Config,
    /// Progress statistics for the current execution
    pub progress: Arc<RwLock<ProgressStats>>,
    transaction_manager: TransactionManager,
    progress_manager: ProgressManager,
    execution_manager: ExecutionManager,
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
        let progress = Arc::new(RwLock::new(ProgressStats::new(0)));

        let transaction_manager = TransactionManager::new(provider.clone(), config.clone());
        let progress_manager = ProgressManager::new(progress.clone());
        let execution_manager =
            ExecutionManager::new(provider.clone(), progress.clone(), config.clone());

        Ok(Self {
            provider,
            operations: None,
            config,
            progress,
            transaction_manager,
            progress_manager,
            execution_manager,
        })
    }

    /// Gets the balance of a wallet
    pub async fn get_wallet_balance(&self, address: &Address) -> Result<U256> {
        self.transaction_manager.get_wallet_balance(address).await
    }

    /// Executes operations in parallel, ensuring parent operations complete before children
    pub async fn parallel_execute_operations(&mut self) -> Result<ExecutionResult> {
        let root_node = self.operations.as_ref().ok_or_else(|| {
            WalletError::WalletOperationError("No operations tree available".to_string())
        })?;

        // Execute all operations in the tree using the task queue approach
        self.execution_manager
            .parallel_execute_operations(root_node.clone(), &self.transaction_manager)
            .await
    }

    /// Prints execution statistics including time, addresses activated, and costs
    pub async fn print_statistics(&self, execution_result: ExecutionResult) -> Result<()> {
        self.progress_manager
            .print_statistics(execution_result, &self.transaction_manager)
            .await
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

        self.transaction_manager
            .analyze_failure_impact(node_id, root_node)
            .await
    }
}
