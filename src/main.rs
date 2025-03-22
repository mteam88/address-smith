use alloy::{
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use alloy_primitives::utils::parse_units;
use dotenv::dotenv;
use env_logger::Builder;
use log::{info, LevelFilter};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use active_address::{
    operations::generate_balanced_split_loops, utils::pretty_print_tree, wallet::WalletManager,
};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv().ok();

    // Create logs directory if it doesn't exist
    fs::create_dir_all("logs")?;

    // Generate unique log file name with timestamp
    let log_file_name = format!(
        "logs/active_address_{}.log",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );

    // Configure logging to write to both console and file
    let mut builder = Builder::from_default_env();
    builder.filter_level(LevelFilter::Info);

    // Create log file
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_name)?;

    builder.format(|buf, record| {
        writeln!(
            buf,
            "[{}] {} - {}",
            record.level(),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            record.args()
        )
    });

    // Write to both console and file
    builder.target(env_logger::Target::Pipe(Box::new(log_file)));
    builder.init();

    info!("Starting new run, logging to: {}", log_file_name);

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

    let mut wallet_manager = WalletManager::new(provider).await?;

    let operations_tree = generate_balanced_split_loops(
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
