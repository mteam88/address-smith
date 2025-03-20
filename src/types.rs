use alloy::{
    network::EthereumWallet,
    primitives::U256,
};
use core::fmt;
use std::fmt::{Debug, Display};
use tokio::time::Duration;

#[derive(Debug, Clone)]
pub struct Operation {
    /// the wallet to draw funds from
    pub from: EthereumWallet,
    /// the wallet to send the funds to
    pub to: EthereumWallet,
    /// if None, the operation will send all available funds - reserving a buffer for gas
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

pub struct ExecutionResult {
    pub new_wallets_count: i32,
    pub initial_balance: U256,
    pub final_balance: U256,
    pub root_wallet: EthereumWallet,
    pub time_elapsed: Duration,
}
