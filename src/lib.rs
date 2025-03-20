//! A library for managing Ethereum wallet operations and transactions.
//!
//! This crate provides functionality for:
//! - Managing multiple Ethereum wallets
//! - Executing sequential and parallel wallet operations
//! - Tracking transaction costs and statistics
//! - Building and executing transaction chains

/// Error types and results for wallet operations
pub mod error;
/// Operations for the wallet manager
pub mod operations;
/// Tree data structure for organizing dependent operations
pub mod tree;
/// Core type definitions for wallet operations
pub mod types;
/// Utility functions for wallet and price operations
pub mod utils;
/// Main wallet management functionality
pub mod wallet;

pub use error::{Result, WalletError};
pub use tree::TreeNode;
pub use types::{ExecutionResult, Operation};
pub use wallet::WalletManager;
