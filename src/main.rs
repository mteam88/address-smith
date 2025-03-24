use alloy::{
    network::EthereumWallet,
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
};
use alloy_primitives::utils::parse_units;
use dotenv::dotenv;
use std::{
    fs::{self},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tracing::{info, Level};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use active_address::{
    operations::generate_balanced_split_loops, utils::pretty_print_tree, wallet::WalletManager,
};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    dotenv().ok();

    // Create logs directory if it doesn't exist
    fs::create_dir_all("logs")?;

    // Generate unique log file name with timestamp
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let log_file_name = format!("active_address_{}.log", timestamp);

    // Set up file appender for logging
    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::NEVER)
        .filename_prefix(format!("active_address_{}", timestamp))
        .filename_suffix("log")
        .build("logs")?;

    // Initialize tracing subscriber with JSON formatting for file only
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            fmt::Layer::new()
                .with_target(true)
                .with_thread_ids(true)
                .with_line_number(true)
                .with_file(true)
                .json()
                .with_span_list(false)
                .with_writer(non_blocking),
        )
        .with(EnvFilter::from_default_env().add_directive(Level::INFO.into()))
        .init();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        log_file = log_file_name,
        "Starting active-address execution"
    );

    let provider = Arc::new(
        ProviderBuilder::new()
            .connect(&dotenv::var("RPC_URL").unwrap())
            .await?,
    );
    provider.client().set_poll_interval(Duration::from_secs(4));
    info!(chain_id = ?provider.get_chain_id().await?, "Connected to provider");

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

    info!(
        to_activate,
        split_loops_count,
        amount_per_wallet = ?amount_per_wallet,
        "Configuration loaded"
    );

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

    info!("Execution completed");
    Ok(())
}
