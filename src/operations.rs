use alloy::network::EthereumWallet;
use alloy_primitives::U256;
use std::path::Path;

use crate::tree::TreeNode;
use crate::types::Operation;
use crate::utils::generate_wallet;

/// Generates a sequence of operations forming a loop starting and ending with the first wallet.
/// Creates a chain where each operation transfers funds to a new wallet, ultimately returning to the first.
/// If a return wallet is provided, the last operation will return to it instead of the first wallet.
pub async fn generate_operation_loop(
    first_wallet: EthereumWallet,
    total_new_wallets: i32,
    return_wallet: Option<EthereumWallet>,
    backup_dir: &Path,
) -> eyre::Result<TreeNode<Operation>> {
    let mut operations = vec![];
    let mut current_wallet = first_wallet.clone();

    for _ in 0..total_new_wallets {
        let next_wallet = generate_wallet(backup_dir).await?;
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
        to: return_wallet.unwrap_or(first_wallet),
        amount: None,
    });
    // Create the root node with the first operation
    let mut root = TreeNode::new(operations[0].clone());

    // Keep track of the current parent node as we build the chain
    let mut current_parent = &mut root;

    // Add each subsequent operation as a child of the previous operation
    for operation in operations.into_iter().skip(1) {
        let new_node = TreeNode::new(operation);
        TreeNode::add_child(current_parent, &new_node);
        // The new node becomes the parent for the next iteration
        current_parent = current_parent.children.last_mut().unwrap();
    }

    Ok(root)
}

/// Generates a parallelizable tree of operations where the first operations create new wallets and each have a loop of children.
///
/// # Arguments
/// * `first_wallet` - The first wallet to start the tree
/// * `total_new_wallets` - The total number of new wallets to create (first layer and all created by children loops)
/// * `total_loops` - The number of loops to create
/// * `amount_per_wallet` - The amount of ether to send to each new first layer wallet
///
/// # Returns
/// * `TreeNode<Operation>` - The root node of the built tree
pub async fn generate_split_loops(
    first_wallet: EthereumWallet,
    total_new_wallets: i32,
    total_loops: i32,
    amount_per_wallet: U256,
    backup_dir: &Path,
) -> eyre::Result<TreeNode<Operation>> {
    // Calculate how many wallets per loop, putting any remainder in the first loop
    let base_wallets_per_loop = (total_new_wallets - total_loops) / total_loops;
    let remainder = (total_new_wallets - total_loops) % total_loops;
    if base_wallets_per_loop < 1 {
        return Err(eyre::eyre!("Not enough wallets to distribute among loops"));
    }

    // Create the root node with a dummy operation that we'll replace
    let mut root = TreeNode::new(Operation {
        from: first_wallet.clone(),
        to: first_wallet.clone(),
        amount: None,
    });

    // Create first layer operations (one for each loop)
    let mut last_op_node = &mut root;
    for loop_index in 0..total_loops {
        // Generate a new wallet for this loop
        let new_wallet = generate_wallet(backup_dir).await?;

        // Create the first operation to fund this wallet
        let first_op = Operation {
            from: first_wallet.clone(),
            to: new_wallet.clone(),
            amount: Some(amount_per_wallet),
        };

        // Create a node for this operation
        let first_op_node = TreeNode::new(first_op);
        TreeNode::add_child(last_op_node, &first_op_node);
        last_op_node = last_op_node.children.last_mut().unwrap();

        // Add remainder wallets to the first loop
        let wallets_for_this_loop = if loop_index == 0 {
            base_wallets_per_loop + remainder
        } else {
            base_wallets_per_loop
        };

        // Generate a loop starting from this new wallet
        let loop_root = generate_operation_loop(
            new_wallet,
            wallets_for_this_loop,
            Some(first_wallet.clone()),
            backup_dir,
        )
        .await?;

        // Add the loop as a child of the first operation
        TreeNode::add_child(last_op_node, &loop_root);
    }

    Ok(root)
}
