use alloy::primitives::{utils::format_units, U256};
use log::info;
use std::{io::Write, sync::Arc};
use tokio::sync::RwLock;

use crate::{
    error::{Result, WalletError},
    types::{ExecutionResult, ProgressStats},
    utils::get_eth_price,
};

use super::transaction::TransactionManager;

/// Manages progress tracking and reporting
pub struct ProgressManager {
    progress: Arc<RwLock<ProgressStats>>,
}

impl ProgressManager {
    /// Creates a new ProgressManager
    pub fn new(progress: Arc<RwLock<ProgressStats>>) -> Self {
        Self { progress }
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

        // Update gas price
        if let Ok(gas_price) = provider.get_gas_price().await {
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

    /// Logs a message
    fn log(&self, message: &str) {
        info!("{}", message);
    }

    /// Prints execution statistics including time, addresses activated, and costs
    pub async fn print_statistics(
        &self,
        execution_result: ExecutionResult,
        transaction_manager: &TransactionManager,
    ) -> Result<()> {
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

                // Use the transaction_manager to analyze the impact
                match transaction_manager
                    .analyze_failure_impact(error.node_id, &execution_result.root_node)
                    .await
                {
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

        let eth_per_wallet = alloy_primitives::utils::parse_units(
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
}
