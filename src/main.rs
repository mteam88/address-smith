use alloy::{
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use alloy_primitives::utils::parse_units;
use dotenv::dotenv;
use log::info;
use std::{path::PathBuf, sync::Arc};

use active_address::{operations::generate_split_loops, wallet::WalletManager, utils::pretty_print_tree};

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

    let operations_tree = generate_split_loops(root_wallet, to_activate, 2, parse_units("1", "ether").unwrap().into(), &PathBuf::from("wallets")).await?;
    pretty_print_tree(&operations_tree);
    wallet_manager.operations = Some(operations_tree);

    let execution_result = wallet_manager.parallel_execute_operations().await?;

    wallet_manager.print_statistics(execution_result).await?;

    Ok(())
}
