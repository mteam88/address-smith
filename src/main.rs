use alloy::{
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use alloy_primitives::utils::parse_units;
use dotenv::dotenv;
use log::info;
use std::{path::PathBuf, sync::Arc, time::Duration};

use active_address::{
    operations::generate_split_loops, utils::pretty_print_tree, wallet::WalletManager,
};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv().ok();
    env_logger::init();

    let provider = Arc::new(
        ProviderBuilder::new()
            .connect(&dotenv::var("RPC_URL").unwrap())
            .await?,
    );
    provider.client().set_poll_interval(Duration::from_secs(4));
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

    let split_loops_count = dotenv::var("SPLIT_LOOPS_COUNT")
        .unwrap_or_else(|_| "2".to_string())
        .parse()
        .unwrap_or(2);

    let amount_per_wallet = parse_units(
        &dotenv::var("AMOUNT_PER_WALLET").unwrap_or_else(|_| "1".to_string()),
        "ether",
    )
    .unwrap()
    .into();

    let mut wallet_manager = WalletManager::new(0, provider).await?;

    let operations_tree = generate_split_loops(
        root_wallet,
        to_activate,
        split_loops_count,
        amount_per_wallet,
        &PathBuf::from("wallets"),
    )
    .await?;
    pretty_print_tree(&operations_tree);
    wallet_manager.operations = Some(operations_tree);

    let execution_result = wallet_manager.parallel_execute_operations().await?;

    wallet_manager.print_statistics(execution_result).await?;

    Ok(())
}
