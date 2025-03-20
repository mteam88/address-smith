//! Core type definitions for wallet operations.
//!
//! This module contains the main types used to represent wallet operations
//! and their execution results.

use alloy::{network::EthereumWallet, primitives::U256};
use core::fmt;
use std::fmt::{Debug, Display};
use tokio::time::Duration;

/// Represents a single transfer operation between two wallets.
/// An operation defines the source wallet, destination wallet, and the amount to transfer.
#[derive(Debug, Clone)]
pub struct Operation {
    /// The wallet to draw funds from
    pub from: EthereumWallet,
    /// The wallet to send the funds to
    pub to: EthereumWallet,
    /// If None, the operation will send all available funds minus a gas buffer
    pub amount: Option<U256>,
}

impl Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Transfer {:?} ETH from {} to {}",
            self.amount,
            self.from.default_signer().address(),
            self.to.default_signer().address()
        )
    }
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
    /// The original wallet that initiated the operation sequence
    pub root_wallet: EthereumWallet,
    /// Total time taken to execute all operations
    pub time_elapsed: Duration,
}
