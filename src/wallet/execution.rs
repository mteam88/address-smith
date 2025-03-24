use alloy_primitives::utils::format_units;
use futures::future;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use tokio::sync::RwLock;
use tracing::{info, info_span, warn};

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{
        create_operation_span, ExecutionResult, NodeError, NodeExecutionResult, Operation,
        ProgressStats,
    },
};

use super::{progress::ProgressManager, transaction::TransactionManager, Config};
use alloy::{network::Ethereum, providers::Provider};

/// Manages execution of operations
pub struct ExecutionManager {
    provider: Arc<dyn Provider<Ethereum>>,
    progress: Arc<RwLock<ProgressStats>>,
}

impl ExecutionManager {
    /// Creates a new ExecutionManager
    pub fn new(
        provider: Arc<dyn Provider<Ethereum>>,
        progress: Arc<RwLock<ProgressStats>>,
        _config: Config,
    ) -> Self {
        Self { provider, progress }
    }

    /// Executes operations in parallel, ensuring parent operations complete before children
    pub async fn parallel_execute_operations(
        &self,
        root_node: TreeNode<Operation>,
        transaction_manager: &TransactionManager,
    ) -> Result<ExecutionResult> {
        let execution_span = info_span!("execution", root_address = %root_node.value.from);
        let _guard = execution_span.enter();

        let start_time = tokio::time::Instant::now();
        let progress_manager = ProgressManager::new(self.progress.clone());

        // Count total operations for progress tracking
        let total_operations = Self::count_total_operations(&root_node);
        *self.progress.write().await = ProgressStats::new(total_operations);

        info!(total_operations, "Starting parallel execution");

        let root_address = root_node.value.from;
        let initial_balance = transaction_manager
            .get_wallet_balance(&root_address)
            .await?;

        info!(
            initial_balance = ?initial_balance,
            "Initial balance retrieved"
        );

        // Spawn a task to print progress updates every 5 seconds
        let progress_clone = progress_manager.clone();
        let progress_handle = tokio::spawn(async move {
            loop {
                progress_clone.print_status_if_needed().await;
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });

        // Execute all operations in the tree using the task queue approach
        let node_result = self
            .execute_operations_with_task_queue(
                root_node.clone(),
                transaction_manager,
                &progress_manager,
            )
            .await?;

        // Abort the progress update task
        progress_handle.abort();

        let final_balance = transaction_manager
            .get_wallet_balance(&root_address)
            .await?;

        info!(
            final_balance = ?final_balance,
            new_wallets = node_result.new_wallets.len(),
            errors = node_result.errors.len(),
            duration = ?start_time.elapsed(),
            "Execution completed"
        );

        Ok(ExecutionResult {
            new_wallets_count: node_result.new_wallets.len() as i32,
            initial_balance,
            final_balance,
            root_address,
            time_elapsed: start_time.elapsed(),
            errors: node_result.errors,
            root_node: root_node.clone(),
        })
    }

    /// Counts total number of operations in the tree using an iterative approach
    fn count_total_operations(node: &TreeNode<Operation>) -> usize {
        let mut count = 0;
        let mut stack = vec![node];

        while let Some(current) = stack.pop() {
            count += 1;
            stack.extend(current.children.iter());
        }

        count
    }

    /// Executes operations using a task queue approach to avoid recursion stack overflow
    /// while maintaining parallel execution of sibling operations.
    ///
    /// This function implements a sophisticated execution strategy:
    /// 1. Uses a task queue to manage operation dependencies
    /// 2. Identifies operations that can be executed in parallel
    /// 3. Maintains parent-child relationships to ensure proper execution order
    /// 4. Detects and handles dependency cycles
    /// 5. Collects execution results and errors
    ///
    /// The task queue approach is used instead of recursion to:
    /// - Avoid stack overflow with deep operation trees
    /// - Enable better control over parallel execution
    /// - Allow for more efficient memory usage
    ///
    /// # Arguments
    /// * `root_node` - The root node of the operation tree
    /// * `transaction_manager` - Manager for transaction operations
    /// * `progress_manager` - Manager for tracking execution progress
    ///
    /// # Returns
    /// * `Result<NodeExecutionResult>` - Execution results including new wallets and errors
    ///
    /// # Errors
    /// * `WalletError::WalletOperationError` - When dependency cycle is detected
    /// * Other errors from transaction execution
    async fn execute_operations_with_task_queue(
        &self,
        root_node: TreeNode<Operation>,
        transaction_manager: &TransactionManager,
        progress_manager: &ProgressManager,
    ) -> Result<NodeExecutionResult> {
        let task_queue_span = info_span!("task_queue");
        let _guard = task_queue_span.enter();

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

        info!(queue_size = 1, "Task queue initialized");

        // Process tasks until the queue is empty
        while !queue.is_empty() {
            // Identify all tasks that are ready to execute in parallel
            let mut ready_tasks = Vec::new();
            let mut pending_tasks = VecDeque::new();

            // Sort tasks into ready and pending
            while let Some(task) = queue.pop_front() {
                let parent_executed = task
                    .parent_id
                    .map(|id| task_map.get(&id).map(|t| t.is_executed).unwrap_or(false))
                    .unwrap_or(true);

                if !task.is_executed && parent_executed {
                    ready_tasks.push(task);
                } else {
                    pending_tasks.push_back(task);
                }
            }

            // If we found no ready tasks but still have pending tasks, we have a dependency cycle
            if ready_tasks.is_empty() && !pending_tasks.is_empty() {
                warn!(
                    pending_tasks = pending_tasks.len(),
                    "Dependency cycle detected"
                );
                return Err(WalletError::WalletOperationError(
                    "Dependency cycle detected in operations".to_string(),
                ));
            }

            // Process all ready tasks in parallel
            if !ready_tasks.is_empty() {
                info!(ready_tasks = ready_tasks.len(), "Processing batch of tasks");

                // Create a future for each ready task
                let futures = ready_tasks
                    .iter()
                    .map(|task| {
                        let node = task.node.clone();
                        let mut task_new_wallets = HashSet::new();
                        let transaction_manager = &transaction_manager;
                        let operation_span = create_operation_span(&node.value, node.id);

                        async move {
                            let _guard = operation_span.enter();

                            info!("Starting operation execution");

                            // Execute the operation
                            let result = self
                                .process_single_operation(
                                    &node.value,
                                    &mut task_new_wallets,
                                    transaction_manager,
                                )
                                .await;

                            if let Err(ref e) = result {
                                warn!(error = %e, "Operation failed");
                            } else {
                                info!("Operation completed successfully");
                            }

                            // Return execution results
                            (node, result, task_new_wallets, task.clone())
                        }
                    })
                    .collect::<Vec<_>>();

                // Execute all ready tasks in parallel
                let results = future::join_all(futures).await;

                // Process results and mark tasks as completed
                for (node, result, task_new_wallets, mut task) in results {
                    let success = result.is_ok();

                    // Update progress stats
                    progress_manager
                        .update_progress(&*self.provider, success)
                        .await;

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

        info!(
            new_wallets = new_wallets.len(),
            errors = errors.len(),
            "Task queue processing completed"
        );

        Ok(NodeExecutionResult {
            new_wallets,
            errors,
        })
    }

    /// Processes a single operation, including checking for new wallets
    async fn process_single_operation(
        &self,
        operation: &Operation,
        new_wallets: &mut HashSet<alloy::primitives::Address>,
        transaction_manager: &TransactionManager,
    ) -> Result<()> {
        let operation_span = info_span!(
            "process_operation",
            from = %operation.from,
            to = %operation.to,
            amount = ?operation.amount.map(|a| format_units(a, "ether").unwrap_or_default()),
        );
        let _guard = operation_span.enter();

        let tx_count = self
            .provider
            .get_transaction_count(operation.from)
            .await
            .map_err(|e| {
                WalletError::ProviderError(format!("Failed to get transaction count: {}", e))
            })?;

        if tx_count == 0 {
            info!(
                address = %operation.from,
                "New wallet detected"
            );
            new_wallets.insert(operation.from);
        }

        if operation.from == operation.to {
            info!("Skipping self-transfer operation");
            return Ok(());
        }

        transaction_manager
            .build_and_send_operation(operation)
            .await
    }
}
