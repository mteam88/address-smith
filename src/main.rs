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
use std::{ops::Mul, sync::Arc};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv().ok();

    let eth_price = get_eth_price().await?;
    println!("ETH Price: {}", eth_price);

    // Create a provider with the HTTP transport using the `reqwest` crate.
    let provider = Arc::new(ProviderBuilder::new().connect(&dotenv::var("RPC_URL").unwrap()).await?);

    println!("Provider Chain ID: {}", provider.get_chain_id().await?);

    // create new wallet from PK
    let signer: PrivateKeySigner = dotenv::var("PRIVATE_KEY").unwrap().parse()?;
    let mut wallet = EthereumWallet::new(signer);   

    // get balance
    let balance = provider.get_balance(wallet.default_signer().address()).await?;
    println!("Starting Balance: {}", format_units(balance, "ether")?);

    let mut wallets = Vec::new();
    wallets.push(wallet.clone());
    let to_activate = dotenv::var("ADDRESS_COUNT").unwrap().parse::<i32>()?;

    assert!(to_activate > 0, "ADDRESS_COUNT must be greater than 0");

    let start = tokio::time::Instant::now();

    for i in 0..(to_activate+1) {
        // if last iteration, send balance back to original wallet
        if i == to_activate {
            send_back(provider.clone(), &wallet, &wallets[0]).await?;
        } else {
            let new_wallet = next(provider.clone(), &wallet).await?;
            wallet = new_wallet;
            wallets.push(wallet.clone());
            println!("Address {}/ activated", i);
        }
    }

    let final_balance = provider.get_balance(wallets[0].default_signer().address()).await?;

    let duration = start.elapsed();
    println!("Total Time Elapsed: {:?}", duration);
    println!("Total Addresses Activated: {}", to_activate);
    println!("Average Time Per Address: {:?}", duration / to_activate as u32);
    println!("Total ETH Cost: {}", format_units(U256::from(balance - final_balance).to::<i128>(), "ether")?);
    println!("Average ETH Cost Per Address: {}", format_units(U256::from(balance - final_balance).to::<i128>(), "ether")?);
    println!("Total USD Cost: {}", format_units(U256::from(balance - final_balance).to::<i128>(), "ether")?.parse::<f64>()? * eth_price);
    println!("Average USD Cost Per Address: {}", format_units(U256::from(balance - final_balance).to::<i128>(), "ether")?.parse::<f64>()? * eth_price / to_activate as f64);


    let balance = provider.get_balance(wallets[0].default_signer().address()).await?;
    println!("Final Balance: {} has been sent back to the original wallet: {}", format_units(balance, "ether")?, wallets[0].default_signer().address());

    Ok(())
}


// Accepts a provider and a wallet, creates a new wallet, sends entire balance to new wallet, returns new wallet
async fn next(provider: Arc<dyn Provider<Ethereum>>, wallet: &EthereumWallet) -> Result<EthereumWallet> {
    // random wallet
    let new_signer = PrivateKeySigner::random();

    // backup private key
    println!("New Wallet Private Key: {}", new_signer.to_bytes());

    let new_wallet: EthereumWallet = EthereumWallet::new(new_signer);

    println!("New Wallet: {}", new_wallet.default_signer().address());
    
    let balance = provider.get_balance(wallet.default_signer().address()).await?;

    // estimate gas
    let gas_price = U256::from(provider.get_gas_price().await?);

    println!("Gas Price: {}", format_units(gas_price, "gwei")?);

    let gas_limit: U256 = U256::from(21000);

    let gas = gas_price * gas_limit;

    println!("Gas: {}", format_units(gas, "gwei")?);

    let value = balance - U256::from(4).mul(gas);

    let tx = TransactionRequest::default()
        .with_from(wallet.default_signer().address())
        .with_to(new_wallet.default_signer().address())
        .with_value(value)
        .with_gas_limit(gas_limit.to::<u64>())
        .with_gas_price(gas_price.to::<u128>())
        .with_nonce(provider.get_transaction_count(wallet.default_signer().address()).await?)
        .with_chain_id(provider.get_chain_id().await?);

    let tx_envelope = tx.clone().build(&wallet).await?;

    // println!("TX: {:?}", tx);
    // println!("TX Envelope: {:?}", tx_envelope);

    println!("Sending transaction...");
    let start = tokio::time::Instant::now();
    let reciept = provider.send_tx_envelope(tx_envelope).await?.get_receipt().await?;
    let duration = start.elapsed();

    println!("Transaction Landed! Time elapsed: {:?}", duration);
    println!("TX Hash: {}", reciept.transaction_hash);
    println!("TX Value: {}", value);
    println!("TX Gas Paid: {}", format_units(U256::from(balance - value).to::<i128>(), "ether")?);
    Ok(new_wallet)
}

async fn send_back(provider: Arc<dyn Provider<Ethereum>>, wallet: &EthereumWallet, to: &EthereumWallet) -> Result<()> {
    let balance = provider.get_balance(wallet.default_signer().address()).await?;

    // estimate gas
    let gas_price = U256::from(provider.get_gas_price().await?);

    println!("Gas Price: {}", format_units(gas_price, "gwei")?);

    let gas_limit: U256 = U256::from(21000);

    let gas = gas_price * gas_limit;

    println!("Gas: {}", format_units(gas, "gwei")?);

    let value = balance - U256::from(4).mul(gas);

    let tx = TransactionRequest::default()
        .with_from(wallet.default_signer().address())
        .with_to(to.default_signer().address())
        .with_value(value)
        .with_gas_limit(gas_limit.to::<u64>())
        .with_gas_price(gas_price.to::<u128>())
        .with_nonce(provider.get_transaction_count(wallet.default_signer().address()).await?)
        .with_chain_id(provider.get_chain_id().await?);

    let tx_envelope = tx.clone().build(&wallet).await?;

    // println!("TX: {:?}", tx);
    // println!("TX Envelope: {:?}", tx_envelope);

    println!("Sending transaction...");
    let start = tokio::time::Instant::now();
    let reciept = provider.send_tx_envelope(tx_envelope).await?.get_receipt().await?;
    let duration = start.elapsed();

    println!("Transaction Landed! Time elapsed: {:?}", duration);
    println!("TX Hash: {}", reciept.transaction_hash);
    println!("TX Value: {}", value);
    println!("TX Gas Paid: {}", format_units(U256::from(balance - value).to::<i128>(), "ether")?);
    Ok(())
}


async fn get_eth_price() -> Result<f64> {
    let response = reqwest::get("https://api.coinbase.com/v2/prices/ETH-USD/spot").await?;
    let body = response.text().await?;
    // parse json
    let parsed: serde_json::Value = serde_json::from_str(&body)?;
    let price = parsed["data"]["amount"].as_str().unwrap().parse::<f64>()?;
    Ok(price)
}
