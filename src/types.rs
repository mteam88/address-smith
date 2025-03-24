//! Core type definitions for wallet operations.
//!
//! This module contains the main types used to represent wallet operations
//! and their execution results.

use alloy::primitives::U256;
use alloy_primitives::{utils::format_units, Address};
use core::fmt;
use std::{
    collections::HashSet,
    fmt::{Debug, Display},
};
use tokio::time::Duration;
use tracing::{span, Level, Span};

use crate::error::WalletError;
use crate::tree::TreeNode;

/// Helper trait for recording fields in tracing spans
pub trait RecordFields {
    fn record_fields(&self) -> Vec<(&'static str, String)>;
}

/// Represents an error that occurred during node execution along with the ID of the node
#[derive(Debug)]
pub struct NodeError {
    /// ID of the node where the error occurred
    pub node_id: usize,
    /// The error that occurred
    pub error: WalletError,
}

impl RecordFields for NodeError {
    fn record_fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("node_id", self.node_id.to_string()),
            ("error", self.error.to_string()),
        ]
    }
}

/// Represents a single transfer operation between two wallets.
/// An operation defines the source wallet, destination wallet, and the amount to transfer.
#[derive(Debug, Clone)]
pub struct Operation {
    /// The wallet to draw funds from
    pub from: Address,
    /// The wallet to send the funds to
    pub to: Address,
    /// If None, the operation will send all available funds minus a gas buffer
    pub amount: Option<U256>,
}

impl RecordFields for Operation {
    fn record_fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = vec![
            ("from", self.from.to_string()),
            ("to", self.to.to_string()),
        ];

        if let Some(amount) = self.amount {
            if let Ok(eth_amount) = format_units(amount, "ether") {
                fields.push(("amount_eth", eth_amount));
            }
            fields.push(("amount_wei", amount.to_string()));
        }

        fields
    }
}

impl Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.from != self.to {
            if let Some(amount) = self.amount {
                write!(
                    f,
                    "Transfer {} ETH from {} to {}",
                    format_units(amount, "ether").unwrap(),
                    self.from,
                    self.to
                )
            } else {
                write!(
                    f,
                    "Transfer all available funds from {} to {}",
                    self.from,
                    self.to
                )
            }
        } else {
            write!(f, "NOOP")
        }
    }
}

/// Creates a new tracing span for an operation
pub fn create_operation_span(operation: &Operation, node_id: usize) -> Span {
    let span = span!(
        Level::INFO,
        "operation",
        node_id = node_id,
        from = %operation.from,
        to = %operation.to,
        amount = ?operation.amount.map(|a| format_units(a, "ether").unwrap_or_default()),
    );
    span
}

/// Result of executing a series of wallet operations.
/// Contains statistics about the execution including balances and timing information.
pub struct ExecutionResult {
    /// Number of new wallets that were activated during execution
    pub new_wallets_count: i32,
    /// Initial balance of the root wallet before operations began
    pub initial_balance: U256,
    /// Final balance of the root wallet after all operations completed
    pub final_balance: U256,
    /// The original address that initiated the operation sequence
    pub root_address: Address,
    /// Total time taken to execute all operations
    pub time_elapsed: Duration,
    /// List of errors that occurred during execution
    pub errors: Vec<NodeError>,
    /// Reference to the root node of the operation tree
    pub root_node: TreeNode<Operation>,
}

pub struct NodeExecutionResult {
    pub new_wallets: HashSet<alloy::primitives::Address>,
    pub errors: Vec<NodeError>,
}

/// Analysis of the impact of a failed operation
#[derive(Debug)]
pub struct FailureImpact {
    /// The node ID that failed
    pub failed_node_id: usize,
    /// Amount of ETH stuck in the source wallet of the failed operation
    pub eth_stuck: U256,
    /// Address where the ETH is stuck
    pub stuck_address: alloy::primitives::Address,
    /// Number of operations that can't proceed due to this failure
    pub orphaned_operations: usize,
    /// List of node IDs that are orphaned due to this failure
    pub orphaned_node_ids: Vec<usize>,
}

impl RecordFields for FailureImpact {
    fn record_fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("failed_node_id", self.failed_node_id.to_string()),
            (
                "eth_stuck",
                format_units(self.eth_stuck, "ether").unwrap_or_default(),
            ),
            ("stuck_address", self.stuck_address.to_string()),
            ("orphaned_operations", self.orphaned_operations.to_string()),
            ("orphaned_node_ids", format!("{:?}", self.orphaned_node_ids)),
        ]
    }
}

impl Display for FailureImpact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Failure Impact Analysis for Node {}:",
            self.failed_node_id
        )?;
        writeln!(
            f,
            "ETH Stuck: {} ETH at address {}",
            format_units(self.eth_stuck, "ether").unwrap_or_else(|_| "ERROR".to_string()),
            self.stuck_address
        )?;
        writeln!(f, "Orphaned Operations: {}", self.orphaned_operations)?;
        writeln!(f, "Orphaned Node IDs: {:?}", self.orphaned_node_ids)?;
        Ok(())
    }
}

/// Tracks progress statistics during execution
#[derive(Debug, Clone)]
pub struct ProgressStats {
    /// Total number of operations to execute
    pub total_operations: usize,
    /// Number of completed operations
    pub completed_operations: usize,
    /// Number of successful operations
    pub successful_operations: usize,
    /// Current gas price in gwei
    pub current_gas_price: U256,
    /// Start time of execution
    pub start_time: std::time::Instant,
    /// Timestamps of operations completed in the last minute
    pub recent_operations: Vec<std::time::Instant>,
}

impl RecordFields for ProgressStats {
    fn record_fields(&self) -> Vec<(&'static str, String)> {
        vec![
            ("total_operations", self.total_operations.to_string()),
            (
                "completed_operations",
                self.completed_operations.to_string(),
            ),
            (
                "successful_operations",
                self.successful_operations.to_string(),
            ),
            (
                "current_gas_price_gwei",
                format_units(self.current_gas_price, "gwei").unwrap_or_default(),
            ),
            ("success_rate", format!("{:.2}%", self.success_rate())),
            (
                "operations_per_minute",
                format!("{:.2}", self.operations_per_minute()),
            ),
        ]
    }
}

impl ProgressStats {
    pub fn new(total_operations: usize) -> Self {
        Self {
            total_operations,
            completed_operations: 0,
            successful_operations: 0,
            current_gas_price: U256::ZERO,
            start_time: std::time::Instant::now(),
            recent_operations: Vec::new(),
        }
    }

    pub fn success_rate(&self) -> f64 {
        if self.completed_operations == 0 {
            100.0
        } else {
            (self.successful_operations as f64 / self.completed_operations as f64) * 100.0
        }
    }

    pub fn operations_per_minute(&self) -> f64 {
        let now = std::time::Instant::now();
        let one_minute_ago = now - std::time::Duration::from_secs(60);

        // Count operations in the last minute
        let recent_count = self
            .recent_operations
            .iter()
            .filter(|&&time| time > one_minute_ago)
            .count();

        if recent_count <= 1 {
            0.0
        } else {
            let oldest_recent = self
                .recent_operations
                .iter()
                .filter(|&&time| time > one_minute_ago)
                .min()
                .copied()
                .unwrap_or(now);

            let window_duration = now - oldest_recent;

            // Ensure we don't divide by a very small duration
            if window_duration.as_secs_f64() < 0.001 {
                0.0
            } else {
                (recent_count as f64) / window_duration.as_secs_f64() * 60.0
            }
        }
    }

    pub fn estimated_time_remaining(&self) -> Option<std::time::Duration> {
        if self.completed_operations == 0 {
            return None;
        }

        let elapsed = self.start_time.elapsed();
        let ops_per_second = self.completed_operations as f64 / elapsed.as_secs_f64();
        let remaining_ops = self
            .total_operations
            .saturating_sub(self.completed_operations);
        let remaining_secs = remaining_ops as f64 / ops_per_second;

        Some(std::time::Duration::from_secs_f64(remaining_secs))
    }
}
