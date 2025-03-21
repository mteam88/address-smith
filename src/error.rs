//! Error types for wallet operations.
//!
//! This module defines the custom error types used throughout the crate
//! and provides a convenient Result type alias.

use alloy_primitives::TxHash;
use thiserror::Error;

/// Errors that can occur during wallet operations.
#[derive(Error, Debug)]
pub enum WalletError {
    /// Environment variable was not found in the .env file
    #[error("Environment variable not found: {0}")]
    EnvVarNotFound(String),

    /// Environment variable had an invalid value
    #[error("Invalid environment variable value: {0}")]
    InvalidEnvVar(String),

    /// Error occurred during transaction execution
    #[error("Transaction error: {0} with hash: {1:?}")]
    TransactionError(String, Option<TxHash>),

    /// Error communicating with the Ethereum provider
    #[error("Provider error: {0}")]
    ProviderError(String),

    /// Error performing file I/O operations
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Error during wallet operation execution
    #[error("Wallet operation error: {0}")]
    WalletOperationError(String),
}

/// A Result type that uses our custom WalletError as the error type.
pub type Result<T> = std::result::Result<T, WalletError>;
