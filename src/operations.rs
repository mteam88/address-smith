use alloy::network::EthereumWallet;
use std::sync::Arc;

use crate::tree::TreeNode;
use crate::types::Operation;
use crate::utils::generate_wallet;

/// Generates a sequence of operations forming a loop starting and ending with the first wallet.
/// Creates a chain where each operation transfers funds to a new wallet, ultimately returning to the first.
pub async fn generate_operation_loop(
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
