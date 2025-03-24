use alloy::primitives::{utils::format_units, U256};
use std::{sync::Arc, time::Instant};
use tokio::sync::RwLock;
use tracing::{info, info_span, warn};

use crate::{
    error::Result,
    types::{ExecutionResult, ProgressStats},
    utils::get_eth_price,
};

use super::transaction::TransactionManager;

/// Manages progress tracking and reporting
#[derive(Clone)]
pub struct ProgressManager {
    progress: Arc<RwLock<ProgressStats>>,
    last_status_update: Arc<RwLock<Instant>>,
}

impl ProgressManager {
    /// Creates a new ProgressManager
    pub fn new(progress: Arc<RwLock<ProgressStats>>) -> Self {
        Self {
            progress,
            last_status_update: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Checks if enough time has passed since the last status update
    async fn should_print_status(&self) -> bool {
        let last_update = *self.last_status_update.read().await;
        last_update.elapsed().as_secs() >= 5
    }

    /// Prints current progress status if enough time has passed since last update
    pub async fn print_status_if_needed(&self) {
        if self.should_print_status().await {
            let progress = self.progress.read().await;

            // Calculate statistics
            let success_rate = progress.success_rate();
            let progress_percent =
                (progress.completed_operations as f64 / progress.total_operations as f64) * 100.0;
            let ops_per_minute = progress.operations_per_minute();
            let time_remaining = progress
                .estimated_time_remaining()
                .map(|d| format!("{:.1} minutes", d.as_secs_f64() / 60.0))
                .unwrap_or_else(|| "calculating...".to_string());

            let gas_price = format_units(progress.current_gas_price, "gwei").unwrap_or_default();

            // Print to terminal
            println!(
                "\nProgress Update:\n\
                 - Completed: {}/{} ({:.1}%)\n\
                 - Success Rate: {:.1}%\n\
                 - Gas Price: {} gwei\n\
                 - Speed: {:.1} ops/minute\n\
                 - Time Remaining: {}\n",
                progress.completed_operations,
                progress.total_operations,
                progress_percent,
                success_rate,
                gas_price,
                ops_per_minute,
                time_remaining
            );

            // Log to tracing
            info!(
                completed = progress.completed_operations,
                total = progress.total_operations,
                progress_percent = format!("{:.1}%", progress_percent),
                success_rate = format!("{:.1}%", success_rate),
                gas_price_gwei = gas_price,
                ops_per_minute = format!("{:.1}", ops_per_minute),
                time_remaining,
                "Progress status"
            );

            // Update last status time
            *self.last_status_update.write().await = Instant::now();
        }
    }

    /// Updates progress statistics and prints current status
    pub async fn update_progress(
        &self,
        provider: &dyn alloy::providers::Provider<alloy::network::Ethereum>,
        success: bool,
    ) {
        let mut progress = self.progress.write().await;
        progress.completed_operations += 1;
        if success {
            progress.successful_operations += 1;
        }

        // Add current operation timestamp to recent operations
        progress.recent_operations.push(std::time::Instant::now());

        // Keep only operations from the last minute
        let one_minute_ago = std::time::Instant::now() - std::time::Duration::from_secs(60);
        progress
            .recent_operations
            .retain(|&time| time > one_minute_ago);

        // Update gas price
        if let Ok(gas_price) = provider.get_gas_price().await {
            progress.current_gas_price = U256::from(gas_price);
        }

        // Print status update if needed
        drop(progress); // Release the write lock before calling print_status
        self.print_status_if_needed().await;
    }

    /// Prints execution statistics including time, addresses activated, and costs
    pub async fn print_statistics(
        &self,
        execution_result: ExecutionResult,
        transaction_manager: &TransactionManager,
    ) -> Result<()> {
        let stats_span = info_span!("execution_statistics");
        let _guard = stats_span.enter();

        let total_duration = execution_result.time_elapsed;
        let eth_spent = execution_result.initial_balance - execution_result.final_balance;
        let eth_price = get_eth_price().await?;
        let eth_spent_str = format_units(eth_spent, "ether").unwrap_or_default();
        let usd_spent = eth_price * eth_spent_str.parse::<f64>().unwrap();

        // Calculate average time per wallet if any wallets were created
        let avg_time_per_wallet = if execution_result.new_wallets_count > 0 {
            total_duration.div_f64(execution_result.new_wallets_count as f64)
        } else {
            total_duration
        };

        // Print to terminal
        println!(
            "\nExecution Summary:\n\
             - Duration: {:?}\n\
             - New Wallets Created: {}\n\
             - Average Time per Wallet: {:?}\n\
             - ETH Spent: {} ETH\n\
             - ETH Price: ${:.2}\n\
             - Total Cost: ${:.2}\n",
            total_duration,
            execution_result.new_wallets_count,
            avg_time_per_wallet,
            eth_spent_str,
            eth_price,
            usd_spent
        );

        // Log to tracing
        info!(
            duration = ?total_duration,
            new_wallets = execution_result.new_wallets_count,
            avg_time_per_wallet = ?avg_time_per_wallet,
            eth_spent = eth_spent_str,
            eth_price = eth_price,
            usd_spent = format!("${:.2}", usd_spent),
            "Execution completed"
        );

        // Print error summary if any errors occurred
        if !execution_result.errors.is_empty() {
            let error_span = info_span!("error_summary");
            let _error_guard = error_span.enter();

            let mut total_eth_stuck = U256::ZERO;
            let mut total_orphaned_ops = 0;

            println!("\nErrors Summary:");
            println!("Total Errors: {}", execution_result.errors.len());

            warn!(
                error_count = execution_result.errors.len(),
                "Errors occurred during execution"
            );

            for error in &execution_result.errors {
                println!("\nOperation Failed (Node {}):", error.node_id);
                println!("Error: {}", error.error);

                warn!(
                    node_id = error.node_id,
                    error = %error.error,
                    "Operation failed"
                );

                // Use the transaction_manager to analyze the impact
                match transaction_manager
                    .analyze_failure_impact(error.node_id, &execution_result.root_node)
                    .await
                {
                    Ok(impact) => {
                        let eth_stuck_str =
                            format_units(impact.eth_stuck, "ether").unwrap_or_default();

                        // Print to terminal
                        println!("Impact:");
                        println!("- ETH Stuck: {} ETH", eth_stuck_str);
                        println!("- Stuck Address: {}", impact.stuck_address);
                        println!("- Orphaned Operations: {}", impact.orphaned_operations);

                        // Log to tracing
                        warn!(
                            node_id = error.node_id,
                            eth_stuck = eth_stuck_str,
                            stuck_address = %impact.stuck_address,
                            orphaned_operations = impact.orphaned_operations,
                            "Failure impact"
                        );

                        total_eth_stuck += impact.eth_stuck;
                        total_orphaned_ops += impact.orphaned_operations;
                    }
                    Err(e) => {
                        println!("Failed to analyze impact: {}", e);
                        warn!(error = %e, "Failed to analyze impact");
                    }
                }
            }

            let total_eth_stuck_str = format_units(total_eth_stuck, "ether").unwrap_or_default();
            let total_eth_stuck_usd = eth_price * total_eth_stuck_str.parse::<f64>().unwrap();

            // Print to terminal
            println!("\nTotal Impact:");
            println!(
                "- Total ETH Stuck: {} ETH (${:.2})",
                total_eth_stuck_str, total_eth_stuck_usd
            );
            println!("- Total Orphaned Operations: {}", total_orphaned_ops);

            // Log to tracing
            warn!(
                total_eth_stuck = total_eth_stuck_str,
                total_eth_stuck_usd = format!("${:.2}", total_eth_stuck_usd),
                total_orphaned_ops,
                "Total impact summary"
            );
        }

        // Calculate and print cost per wallet
        if execution_result.new_wallets_count > 0 {
            let eth_per_wallet =
                eth_spent_str.parse::<f64>().unwrap() / execution_result.new_wallets_count as f64;
            let usd_per_wallet = eth_price * eth_per_wallet;

            // Print to terminal
            println!("\nCost per Wallet:");
            println!("- ETH: {:.6} ETH", eth_per_wallet);
            println!("- USD: ${:.2}", usd_per_wallet);

            // Log to tracing
            info!(
                eth_per_wallet = format!("{:.6} ETH", eth_per_wallet),
                usd_per_wallet = format!("${:.2}", usd_per_wallet),
                "Cost per wallet"
            );
        }

        Ok(())
    }
}
