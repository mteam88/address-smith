use alloy::primitives::{utils::format_units, U256};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, info_span, warn};

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

        info!(
            completed = progress.completed_operations,
            total = progress.total_operations,
            progress_percent = format!("{:.1}%", progress_percent),
            success_rate = format!("{:.1}%", success_rate),
            gas_price_gwei = ?format_units(progress.current_gas_price, "gwei").unwrap_or_default(),
            ops_per_minute = format!("{:.1}", ops_per_minute),
            time_remaining,
            "Progress update"
        );
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

        info!(
            duration = ?total_duration,
            new_wallets = execution_result.new_wallets_count,
            eth_spent = ?format_units(eth_spent, "ether").unwrap_or_default(),
            eth_price = eth_price,
            usd_spent = format!("${:.2}", eth_price * format_units(eth_spent, "ether").unwrap_or_default().parse::<f64>().unwrap()),
            "Execution completed"
        );

        // Print error summary if any errors occurred
        if !execution_result.errors.is_empty() {
            let error_span = info_span!("error_summary");
            let _error_guard = error_span.enter();

            warn!(
                error_count = execution_result.errors.len(),
                "Errors occurred during execution"
            );

            // Track total impact
            let mut total_eth_stuck = U256::ZERO;
            let mut total_orphaned_ops = 0;

            for error in &execution_result.errors {
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
                        warn!(
                            node_id = error.node_id,
                            eth_stuck = ?format_units(impact.eth_stuck, "ether").unwrap_or_default(),
                            stuck_address = %impact.stuck_address,
                            orphaned_operations = impact.orphaned_operations,
                            "Failure impact"
                        );
                        total_eth_stuck += impact.eth_stuck;
                        total_orphaned_ops += impact.orphaned_operations;
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to analyze impact");
                    }
                }
            }

            // Print total impact statistics
            warn!(
                total_eth_stuck = ?format_units(total_eth_stuck, "ether").unwrap_or_default(),
                total_eth_stuck_usd = format!("${:.2}", eth_price * format_units(total_eth_stuck, "ether").unwrap_or_default().parse::<f64>().unwrap()),
                total_orphaned_ops,
                "Total impact summary"
            );
        }

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

        info!(
            eth_per_wallet = ?format_units(eth_per_wallet, "ether").unwrap_or_default(),
            usd_per_wallet = format!("${:.2}", eth_price * format_units(eth_per_wallet, "ether").unwrap_or_default().parse::<f64>().unwrap()),
            "Cost per wallet"
        );

        Ok(())
    }
}
