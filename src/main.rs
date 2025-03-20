use alloy::{
    network::{EthereumWallet, TransactionBuilder, Ethereum},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    primitives::{
        U256,
        utils::format_units,
    },
};
use eyre::Result;
use dotenv::dotenv;
use std::{ops::Mul, sync::Arc, io::Write, path::PathBuf};
use log::{info, error};
use tokio::task::JoinSet;

const GAS_LIMIT: u64 = 21000;
const GAS_BUFFER_MULTIPLIER: u64 = 4;

struct WalletManager {
    id: usize,
    provider: Arc<dyn Provider<Ethereum>>,
    wallets: Vec<EthereumWallet>,
    current_wallet: EthereumWallet,
    start_time: tokio::time::Instant,
    initial_balance: U256,
    log_file: PathBuf,
}

impl WalletManager {
    async fn new(
        id: usize,
        provider: Arc<dyn Provider<Ethereum>>, 
        initial_wallet: EthereumWallet,
        target_balance: U256,
    ) -> Result<Self> {
        let log_file = PathBuf::from(format!("wallet_manager_{}.log", id));
        
        Ok(Self {
            id,
            provider,
            wallets: vec![initial_wallet.clone()],
            current_wallet: initial_wallet,
            start_time: tokio::time::Instant::now(),
            initial_balance: target_balance,
            log_file,
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

    async fn build_transaction(&self, to_address: alloy::primitives::Address, value: U256) -> Result<TransactionRequest> {
        let gas_price = U256::from(self.provider.get_gas_price().await?);
        let nonce = self.provider.get_transaction_count(self.current_wallet.default_signer().address()).await?;
        let chain_id = self.provider.get_chain_id().await?;

        Ok(TransactionRequest::default()
            .with_from(self.current_wallet.default_signer().address())
            .with_to(to_address)
            .with_value(value)
            .with_gas_limit(GAS_LIMIT)
            .with_gas_price(gas_price.to::<u128>())
            .with_nonce(nonce)
            .with_chain_id(chain_id))
    }

    async fn activate_next_wallet(&mut self) -> Result<()> {
        let new_signer = PrivateKeySigner::random();
        self.log(&format!("New Wallet Private Key: {}", new_signer.to_bytes()))?;

        let new_wallet = EthereumWallet::new(new_signer);
        self.log(&format!("New Wallet: {}", new_wallet.default_signer().address()))?;

        let balance = self.provider.get_balance(self.current_wallet.default_signer().address()).await?;
        let gas_price = U256::from(self.provider.get_gas_price().await?);
        let gas = gas_price * U256::from(GAS_LIMIT);
        let value = balance - U256::from(GAS_BUFFER_MULTIPLIER).mul(gas);

        let tx = self.build_transaction(new_wallet.default_signer().address(), value).await?;
        let tx_envelope = tx.build(&self.current_wallet).await?;

        self.log("Sending transaction...")?;
        let start = tokio::time::Instant::now();
        let receipt = self.provider.send_tx_envelope(tx_envelope).await?.get_receipt().await?;
        let duration = start.elapsed();

        self.log(&format!("Transaction Landed! Time elapsed: {:?}", duration))?;
        self.log(&format!("TX Hash: {}", receipt.transaction_hash))?;
        self.log(&format!("TX Value: {}", value))?;
        self.log(&format!("TX Gas Paid: {}", format_units(U256::from(balance - value).to::<i128>(), "ether")?))?;

        self.wallets.push(new_wallet.clone());
        self.current_wallet = new_wallet;
        Ok(())
    }

    async fn send_back_to_original(&self) -> Result<()> {
        let balance = self.provider.get_balance(self.current_wallet.default_signer().address()).await?;
        let gas_price = U256::from(self.provider.get_gas_price().await?);
        let gas = gas_price * U256::from(GAS_LIMIT);
        let value = balance - U256::from(GAS_BUFFER_MULTIPLIER).mul(gas);

        let tx = self.build_transaction(self.wallets[0].default_signer().address(), value).await?;
        let tx_envelope = tx.build(&self.current_wallet).await?;

        self.log("Sending final balance back to original wallet...")?;
        let start = tokio::time::Instant::now();
        let receipt = self.provider.send_tx_envelope(tx_envelope).await?.get_receipt().await?;
        let duration = start.elapsed();

        self.log(&format!("Transaction Landed! Time elapsed: {:?}", duration))?;
        self.log(&format!("TX Hash: {}", receipt.transaction_hash))?;
        self.log(&format!("TX Value: {}", value))?;
        self.log(&format!("TX Gas Paid: {}", format_units(U256::from(balance - value).to::<i128>(), "ether")?))?;
        Ok(())
    }

    async fn print_statistics(&self, address_count: i32) -> Result<()> {
        let final_balance = self.provider.get_balance(self.wallets[0].default_signer().address()).await?;
        let total_duration = self.start_time.elapsed();
        let eth_spent = self.initial_balance - final_balance;
        let eth_price = get_eth_price().await?;
        
        self.log("\n========================================\nActivation Complete!\n========================================\n")?;
        
        self.log(&format!("Total Time Elapsed: {:?}", total_duration))?;
        self.log(&format!("Total Addresses Activated: {}", address_count))?;
        self.log(&format!("Average Time Per Address: {:?}", total_duration / address_count as u32))?;
        self.log(&format!("Total ETH Cost: {}", format_units(eth_spent.to::<i128>(), "ether")?))?;
        self.log(&format!("Average ETH Cost Per Address: {}", format_units(eth_spent.to::<i128>() / address_count as i128, "ether")?))?;
        self.log(&format!("Total USD Cost: {}", format_units(eth_spent.to::<i128>(), "ether")?.parse::<f64>()? * eth_price))?;
        self.log(&format!("Average USD Cost Per Address: {}", format_units(eth_spent.to::<i128>(), "ether")?.parse::<f64>()? * eth_price / address_count as f64))?;
        self.log(&format!("Final Balance: {} has been sent back to the original wallet: {}", 
            format_units(final_balance, "ether")?, 
            self.wallets[0].default_signer().address()
        ))?;
        Ok(())
    }

    async fn run(&mut self, address_count: i32) -> Result<()> {
        self.log(&format!("Starting wallet manager {} with target of {} addresses", self.id, address_count))?;
        
        for i in 0..address_count {
            self.activate_next_wallet().await?;
            self.log(&format!("Address {}/{} activated", i+1, address_count))?;
        }

        self.send_back_to_original().await?;
        self.print_statistics(address_count).await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv().ok();
    env_logger::init();

    let provider = Arc::new(ProviderBuilder::new().connect(&dotenv::var("RPC_URL").unwrap()).await?);
    info!("Provider Chain ID: {}", provider.get_chain_id().await?);

    // Parse initial balances from environment
    let initial_balances: Vec<f64> = dotenv::var("INITIAL_BALANCES")
        .unwrap_or_else(|_| "1.0".to_string())
        .split(',')
        .map(|s| s.trim().parse::<f64>().unwrap())
        .collect();

    let private_keys: Vec<String> = dotenv::var("PRIVATE_KEYS")
        .expect("PRIVATE_KEYS must be set in .env")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    assert_eq!(
        initial_balances.len(),
        private_keys.len(),
        "Number of initial balances must match number of private keys"
    );

    let to_activate = dotenv::var("ADDRESS_COUNT").unwrap().parse::<i32>()?;
    assert!(to_activate > 0, "ADDRESS_COUNT must be greater than 0");

    let mut join_set = JoinSet::new();

    for (id, (balance, private_key)) in initial_balances.iter().zip(private_keys.iter()).enumerate() {
        let provider = Arc::clone(&provider);
        let signer: PrivateKeySigner = private_key.parse()?;
        let initial_wallet = EthereumWallet::new(signer);
        let target_balance = U256::from((*balance * 1e18) as u128);

        let mut wallet_manager = WalletManager::new(id, provider, initial_wallet, target_balance).await?;
        
        join_set.spawn(async move {
            if let Err(e) = wallet_manager.run(to_activate).await {
                error!("Wallet manager {} failed: {}", id, e);
                Err(e)
            } else {
                Ok(())
            }
        });
    }

    while let Some(result) = join_set.join_next().await {
        result??;
    }

    Ok(())
}

async fn get_eth_price() -> Result<f64> {
    let response = reqwest::get("https://api.coinbase.com/v2/prices/ETH-USD/spot").await?;
    let body = response.text().await?;
    let parsed: serde_json::Value = serde_json::from_str(&body)?;
    let price = parsed["data"]["amount"].as_str().unwrap().parse::<f64>()?;
    Ok(price)
}
