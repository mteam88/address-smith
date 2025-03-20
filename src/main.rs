use alloy::{
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{utils::format_units, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::{LocalSigner, PrivateKeySigner},
};
use dotenv::dotenv;
use eyre::Result;
use log::info;
use std::{io::Write, ops::Mul, path::PathBuf, sync::Arc, sync::Mutex};

const GAS_LIMIT: u64 = 21000;
const GAS_BUFFER_MULTIPLIER: u64 = 4;

struct WalletManager {
    id: usize,
    provider: Arc<dyn Provider<Ethereum>>,
    /// Operations tree. Every sub-operation is dependent on the completion of it's parent operation.
    operations: Option<Arc<Mutex<TreeNode<Operation>>>>,
    start_time: tokio::time::Instant,
    activated_wallets: Arc<Mutex<usize>>,
    log_file: PathBuf,
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

impl WalletManager {
    async fn new(id: usize, provider: Arc<dyn Provider<Ethereum>>) -> Result<Self> {
        let log_file = PathBuf::from(format!("wallet_manager_{}.log", id));

        Ok(Self {
            id,
            provider: provider.clone(),
            operations: None,
            start_time: tokio::time::Instant::now(),
            activated_wallets: Arc::new(Mutex::new(0)),
            log_file,
        })
    }

    /// Execute all operations, keeping dependencies in mind.
    async fn parallel_execute_operations(self) -> Result<()> {
        // TODO: Implement
        Ok(())
    }

    async fn sequential_execute_operations(self) -> Result<()> {
        // flatten the operations tree, and execute each operation in order
        let operations = self.operations.as_ref().unwrap();
        let operations = operations.lock().unwrap();
        let mut operations_list = vec![];
        for operation in operations.children.iter() {
            operations_list.push(operation.lock().unwrap().value.clone());
        }
        for operation in operations_list {
            self.build_and_send_operation(operation).await?;
        }
        Ok(())
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

    async fn print_statistics(
        &self,
        address_count: i32,
        initial_balance: U256,
        final_balance: U256,
        root_wallet: EthereumWallet,
    ) -> Result<()> {
        let total_duration = self.start_time.elapsed();
        let eth_spent = initial_balance - final_balance;
        let eth_price = get_eth_price().await?;

        self.log("\n========================================\nActivation Complete!\n========================================\n")?;

        self.log(&format!("Total Time Elapsed: {:?}", total_duration))?;
        self.log(&format!("Total Addresses Activated: {}", address_count))?;
        self.log(&format!(
            "Average Time Per Address: {:?}",
            total_duration / address_count as u32
        ))?;
        self.log(&format!(
            "Total ETH Cost: {}",
            format_units(eth_spent.to::<i128>(), "ether")?
        ))?;
        self.log(&format!(
            "Average ETH Cost Per Address: {}",
            format_units(eth_spent.to::<i128>() / address_count as i128, "ether")?
        ))?;
        self.log(&format!(
            "Total USD Cost: {}",
            format_units(eth_spent.to::<i128>(), "ether")?.parse::<f64>()? * eth_price
        ))?;
        self.log(&format!(
            "Average USD Cost Per Address: {}",
            format_units(eth_spent.to::<i128>(), "ether")?.parse::<f64>()? * eth_price
                / address_count as f64
        ))?;
        self.log(&format!(
            "Final Balance: {} has been sent back to the original wallet: {}",
            format_units(final_balance, "ether")?,
            root_wallet.default_signer().address()
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

    wallet_manager.sequential_execute_operations().await?;

    Ok(())
}

async fn generate_operation_loop(
    first_wallet: EthereumWallet,
    total_new_wallets: i32,
) -> Result<Arc<Mutex<TreeNode<Operation>>>> {
    let mut operations = vec![];
    for _ in 0..total_new_wallets {
        let wallet = generate_wallet().await?;
        let operation = Operation {
            from: first_wallet.clone(),
            to: wallet,
            amount: None,
        };
        operations.push(operation);
    }
    operations.push(Operation {
        from: operations.last().unwrap().to.clone(),
        to: first_wallet.clone(),
        amount: None,
    });

    // Create the root node with the first operation
    let root = TreeNode::new(operations[0].clone());

    // Create a chain of operations where each operation is a child of the previous one
    let mut current = root.clone();
    for operation in operations.into_iter().skip(1) {
        let new_node = TreeNode::new(operation);
        TreeNode::add_child(current.clone(), new_node.clone());
        current = new_node;
    }

    println!("Root: {:?}", root);

    Ok(root)
}

async fn generate_wallet() -> Result<EthereumWallet> {
    let signer = LocalSigner::new(rand::thread_rng());
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

impl<T> TreeNode<T> {
    fn new(value: T) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(TreeNode {
            value,
            children: Vec::new(),
        }))
    }

    fn add_child(parent: Arc<Mutex<Self>>, child: Arc<Mutex<TreeNode<T>>>) {
        parent.lock().unwrap().children.push(child);
    }
}
