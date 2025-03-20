use alloy::{
    network::EthereumWallet, providers::{Provider, ProviderBuilder}, signers::local::PrivateKeySigner,
};
use dotenv::dotenv;
use log::info;
use std::sync::Arc;

use active_address::{
    tree::TreeNode, types::Operation, utils::generate_wallet, wallet::WalletManager,
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

/// Generates a sequence of operations forming a loop starting and ending with the first wallet.
/// Creates a chain where each operation transfers funds to a new wallet, ultimately returning to the first.
async fn generate_operation_loop(
    first_wallet: EthereumWallet,
    total_new_wallets: i32,
) -> eyre::Result<Arc<std::sync::Mutex<TreeNode<Operation>>>> {
    let mut operations = vec![];
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
