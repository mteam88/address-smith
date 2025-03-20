use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{map::HashSet, utils::format_units, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use core::fmt;
use dotenv::dotenv;
use eyre::Result;
use log::info;
use std::{
    fmt::{Debug, Display},
    io::Write,
    ops::Mul,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::time::Duration;

const GAS_LIMIT: u64 = 21000;
const GAS_BUFFER_MULTIPLIER: u64 = 2;

struct WalletManager {
    id: usize,
    provider: Arc<dyn Provider<Ethereum>>,
    /// Operations tree. Every sub-operation is dependent on the completion of it's parent operation.
    operations: Option<Arc<Mutex<TreeNode<Operation>>>>,
    log_file: PathBuf,
}

struct ExecutionResult {
    new_wallets_count: i32,
    initial_balance: U256,
    final_balance: U256,
    root_wallet: EthereumWallet,
    time_elapsed: Duration,
}

/// an operation is an amount of funds to send to a wallet, from another wallet.
#[derive(Debug, Clone)]
struct Operation {
    /// the wallet to draw funds from
    from: EthereumWallet,
    /// the wallet to send the funds to
    to: EthereumWallet,
    /// if None, the operation will send all available funds - reserving a buffer for gas
    amount: Option<U256>,
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

impl WalletManager {
    async fn new(id: usize, provider: Arc<dyn Provider<Ethereum>>) -> Result<Self> {
        let log_file = PathBuf::from(format!("wallet_manager_{}.log", id));

        Ok(Self {
            id,
            provider: provider.clone(),
            operations: None,
            log_file,
        })
    }

    /// Execute all operations, keeping dependencies in mind.
    async fn parallel_execute_operations(self) -> Result<()> {
        // TODO: Implement
        Ok(())
    }

    async fn sequential_execute_operations(&mut self) -> Result<ExecutionResult> {
        let start_time = tokio::time::Instant::now();

        let operations = self.operations.as_ref().unwrap();
        let operations_list = {
            let operations = operations.lock().unwrap();
            operations.flatten()
        }; // MutexGuard is dropped here

        let root_wallet = operations_list.first().unwrap().from.clone();
        assert!(
            root_wallet.default_signer().address()
                == operations_list
                    .last()
                    .unwrap()
                    .to
                    .default_signer()
                    .address(),
            "Root wallet address does not match last operation's to address"
        );

        let initial_balance = self
            .provider
            .get_balance(root_wallet.default_signer().address())
            .await?;
        let mut new_wallets = HashSet::new();

        for operation in operations_list {
            self.log(&format!("Executing operation: {}", operation))?;
            if self
                .provider
                .get_transaction_count(operation.from.default_signer().address())
                .await?
                == 0
            {
                new_wallets.insert(operation.from.default_signer().address());
            }
            self.build_and_send_operation(operation).await?;
        }

        let new_wallets_count = new_wallets.len() as i32;
        let final_balance = self
            .provider
            .get_balance(root_wallet.default_signer().address())
            .await?;

        Ok(ExecutionResult {
            new_wallets_count,
            initial_balance,
            final_balance,
            root_wallet,
            time_elapsed: start_time.elapsed(),
        })
    }

    fn log(&self, message: &str) -> Result<()> {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let log_message = format!("[{}] {}\n", timestamp, message);

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)?;

        file.write_all(log_message.as_bytes())?;
        info!("[Manager {}] {}", self.id, message);
        Ok(())
    }

    async fn build_transaction(
        &self,
        from_wallet: EthereumWallet,
        to_address: alloy::primitives::Address,
        value: Option<U256>,
    ) -> Result<TransactionRequest> {
        let gas_price = U256::from(self.provider.get_gas_price().await?);
        let nonce = self
            .provider
            .get_transaction_count(from_wallet.default_signer().address())
            .await?;
        let chain_id = self.provider.get_chain_id().await?;

        let max_value = U256::from(
            self.provider
                .get_balance(from_wallet.default_signer().address())
                .await?,
        );
        let gas = gas_price * U256::from(GAS_LIMIT);
        let max_value = max_value - U256::from(GAS_BUFFER_MULTIPLIER).mul(gas);

        let value = match value {
            Some(v) => v,
            None => max_value,
        };

        Ok(TransactionRequest::default()
            .with_from(from_wallet.default_signer().address())
            .with_to(to_address)
            .with_value(value)
            .with_gas_limit(GAS_LIMIT)
            .with_gas_price(gas_price.to::<u128>())
            .with_nonce(nonce)
            .with_chain_id(chain_id))
    }

    async fn send_transaction(&self, tx: TransactionRequest, wallet: EthereumWallet) -> Result<()> {
        self.log("Sending transaction...")?;
        let tx_envelope = tx.clone().build(&wallet).await?;

        let start = tokio::time::Instant::now();
        let receipt = self
            .provider
            .send_tx_envelope(tx_envelope)
            .await?
            .get_receipt()
            .await?;
        let duration = start.elapsed();

        self.log(&format!("Transaction Landed! Time elapsed: {:?}", duration))?;
        self.log(&format!("TX Hash: {}", receipt.transaction_hash))?;
        self.log(&format!(
            "TX Value: {}",
            format_units(tx.value.unwrap(), "ether")?
        ))?;
        // self.log(&format!("TX Gas Paid: {}", format_units(U256::from(balance - tx.value).to::<i128>(), "ether")?))?;
        Ok(())
    }

    async fn build_and_send_operation(&self, operation: Operation) -> Result<()> {
        let tx = self
            .build_transaction(
                operation.from.clone(),
                operation.to.default_signer().address(),
                operation.amount,
            )
            .await?;
        self.send_transaction(tx, operation.from.clone()).await?;
        Ok(())
    }

    async fn print_statistics(&self, execution_result: ExecutionResult) -> Result<()> {
        let total_duration = execution_result.time_elapsed;
        let eth_spent = execution_result.initial_balance - execution_result.final_balance;
        let eth_price = get_eth_price().await?;

        self.log("\n========================================\nActivation Complete!\n========================================\n")?;

        self.log(&format!("Total Time Elapsed: {:?}", total_duration))?;
        self.log(&format!(
            "Total Addresses Activated: {}",
            execution_result.new_wallets_count
        ))?;
        self.log(&format!(
            "Average Time Per Address: {:?}",
            total_duration / execution_result.new_wallets_count as u32
        ))?;
        self.log(&format!(
            "Total ETH Cost: {}",
            format_units(eth_spent.to::<i128>(), "ether")?
        ))?;
        self.log(&format!(
            "Average ETH Cost Per Address: {}",
            format_units(
                eth_spent.to::<i128>() / execution_result.new_wallets_count as i128,
                "ether"
            )?
        ))?;
        self.log(&format!(
            "Total USD Cost: {}",
            format_units(eth_spent.to::<i128>(), "ether")?.parse::<f64>()? * eth_price
        ))?;
        self.log(&format!(
            "Average USD Cost Per Address: {}",
            format_units(eth_spent.to::<i128>(), "ether")?.parse::<f64>()? * eth_price
                / execution_result.new_wallets_count as f64
        ))?;
        self.log(&format!(
            "Final Balance: {} has been sent back to the original wallet: {}",
            format_units(execution_result.final_balance, "ether")?,
            execution_result.root_wallet.default_signer().address()
        ))?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv().ok();
    env_logger::init();

    let provider = Arc::new(
        ProviderBuilder::new()
            .connect(&dotenv::var("RPC_URL").unwrap())
            .await?,
    );
    info!("Provider Chain ID: {}", provider.get_chain_id().await?);

    let private_key: String = dotenv::var("PRIVATE_KEY")
        .expect("PRIVATE_KEY must be set in .env")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let signer: PrivateKeySigner = private_key.parse()?;
    let root_wallet = EthereumWallet::new(signer);

    let to_activate = dotenv::var("ADDRESS_COUNT").unwrap().parse::<i32>()?;
    assert!(to_activate > 0, "ADDRESS_COUNT must be greater than 0");

    let mut wallet_manager = WalletManager::new(0, provider).await?;

    let operations_tree = generate_operation_loop(root_wallet, to_activate).await?;
    wallet_manager.operations = Some(operations_tree);

    let execution_result = wallet_manager.sequential_execute_operations().await?;

    wallet_manager.print_statistics(execution_result).await?;

    Ok(())
}

async fn generate_operation_loop(
    first_wallet: EthereumWallet,
    total_new_wallets: i32,
) -> Result<Arc<Mutex<TreeNode<Operation>>>> {
    let mut operations = vec![];
    // Create a chain of operations where each operation is a child of the previous one, each operation sends all ETH to the next wallet
    let mut current_wallet = first_wallet.clone();
    for _ in 0..total_new_wallets {
        let next_wallet = generate_wallet().await?;
        let operation = Operation {
            from: current_wallet,
            to: next_wallet.clone(),
            amount: None,
        };
        operations.push(operation);
        current_wallet = next_wallet;
    }
    operations.push(Operation {
        from: operations.last().unwrap().to.clone(),
        to: first_wallet.clone(),
        amount: None,
    });

    println!("Operations:");
    for (i, op) in operations.iter().enumerate() {
        println!("  {}: {}", i + 1, op);
    }

    // Create the root node with the first operation
    let root = TreeNode::new(operations[0].clone());

    // Create a chain of operations where each operation is a child of the previous one
    let mut current = root.clone();
    for operation in operations.into_iter().skip(1) {
        let new_node = TreeNode::new(operation);
        TreeNode::add_child(current.clone(), new_node.clone());
        current = new_node;
    }

    Ok(root)
}

async fn generate_wallet() -> Result<EthereumWallet> {
    let signer = PrivateKeySigner::random();
    let wallet = EthereumWallet::new(signer);
    Ok(wallet)
}

async fn get_eth_price() -> Result<f64> {
    let response = reqwest::get("https://api.coinbase.com/v2/prices/ETH-USD/spot").await?;
    let body = response.text().await?;
    let parsed: serde_json::Value = serde_json::from_str(&body)?;
    let price = parsed["data"]["amount"].as_str().unwrap().parse::<f64>()?;
    Ok(price)
}

#[derive(Debug)]
struct TreeNode<T> {
    value: T,
    children: Vec<Arc<Mutex<TreeNode<T>>>>,
}

impl<T: Clone + Debug> TreeNode<T> {
    fn new(value: T) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(TreeNode {
            value,
            children: Vec::new(),
        }))
    }

    fn add_child(parent: Arc<Mutex<Self>>, child: Arc<Mutex<TreeNode<T>>>) {
        parent.lock().unwrap().children.push(child);
    }

    /// Flattens the tree so that any parent operation is executed before any of it's children.
    fn flatten(&self) -> Vec<T> {
        let mut operations_list = vec![];
        operations_list.push(self.value.clone());
        for child in self.children.iter() {
            // operations_list.push(child.lock().unwrap().value.clone());
            operations_list.extend(child.lock().unwrap().flatten());
        }
        operations_list
    }
}
