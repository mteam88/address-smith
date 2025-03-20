use thiserror::Error;

#[derive(Error, Debug)]
pub enum WalletError {
    #[error("Environment variable not found: {0}")]
    EnvVarNotFound(String),

    #[error("Invalid environment variable value: {0}")]
    InvalidEnvVar(String),

    #[error("Transaction error: {0}")]
    TransactionError(String),

    #[error("Provider error: {0}")]
    ProviderError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Wallet operation error: {0}")]
    WalletOperationError(String),
}

pub type Result<T> = std::result::Result<T, WalletError>;
