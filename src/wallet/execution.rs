use futures::future;
use log::info;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use tokio::sync::RwLock;

use crate::{
    error::{Result, WalletError},
    tree::TreeNode,
    types::{ExecutionResult, NodeError, NodeExecutionResult, Operation, ProgressStats},
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

    /// Logs a message
    fn log(&self, message: &str) {
        info!("{}", message);
    }

    /// Executes operations in parallel, ensuring parent operations complete before children
    pub async fn parallel_execute_operations(
        &self,
        root_node: TreeNode<Operation>,
        transaction_manager: &TransactionManager,
    ) -> Result<ExecutionResult> {
        let start_time = tokio::time::Instant::now();
        let progress_manager = ProgressManager::new(self.progress.clone());

        // Count total operations for progress tracking
        let total_operations = Self::count_total_operations(&root_node);
        *self.progress.write().await = ProgressStats::new(total_operations);

        let root_wallet = root_node.value.from.clone();
        let initial_balance = transaction_manager.get_wallet_balance(&root_wallet).await?;

        // Execute all operations in the tree using the task queue approach
        let node_result = self
            .execute_operations_with_task_queue(
                root_node.clone(),
                transaction_manager,
                &progress_manager,
            )
            .await?;

        let final_balance = transaction_manager.get_wallet_balance(&root_wallet).await?;

        Ok(ExecutionResult {
            new_wallets_count: node_result.new_wallets.len() as i32,
            initial_balance,
            final_balance,
            root_wallet,
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
        transaction_manager: &TransactionManager,
        progress_manager: &ProgressManager,
    ) -> Result<NodeExecutionResult> {
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
                        let transaction_manager = &transaction_manager;

                        async move {
                            let operation = node.value.clone();

                            // Execute the operation
                            let result = self
                                .process_single_operation(
                                    &operation,
                                    &mut task_new_wallets,
                                    transaction_manager,
                                )
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

        transaction_manager
            .build_and_send_operation(operation)
            .await
    }
}
