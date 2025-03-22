use alloy::network::EthereumWallet;
use alloy_primitives::U256;
use std::path::Path;

use crate::tree::TreeNode;
use crate::types::Operation;
use crate::utils::generate_wallet;

/// Creates a root node with a dummy operation from a wallet to itself
fn create_dummy_root(wallet: &EthereumWallet) -> TreeNode<Operation> {
    TreeNode::new(Operation {
        from: wallet.clone(),
        to: wallet.clone(),
        amount: None,
    })
}

/// Creates a funding operation node that sends funds from the first wallet to a new wallet
async fn create_funding_operation(
    first_wallet: &EthereumWallet,
    amount: U256,
    backup_dir: &Path,
) -> eyre::Result<(TreeNode<Operation>, EthereumWallet)> {
    let new_wallet = generate_wallet(backup_dir).await?;
    let operation = Operation {
        from: first_wallet.clone(),
        to: new_wallet.clone(),
        amount: Some(amount),
    };
    Ok((TreeNode::new(operation), new_wallet))
}

/// Creates a loop starting from a wallet and adds it as a child of the given parent node
async fn attach_loop_to_parent(
    parent: &mut TreeNode<Operation>,
    start_wallet: EthereumWallet,
    wallets_in_loop: i32,
    return_wallet: EthereumWallet,
    backup_dir: &Path,
) -> eyre::Result<()> {
    let loop_root = generate_operation_loop(
        start_wallet,
        wallets_in_loop,
        Some(return_wallet),
        backup_dir,
    )
    .await?;
    TreeNode::add_child(parent, &loop_root);
    Ok(())
}

/// Generates a sequence of operations forming a loop starting and ending with the first wallet.
/// Creates a chain where each operation transfers funds to a new wallet, ultimately returning to the first.
/// If a return wallet is provided, the last operation will return to it instead of the first wallet.
pub async fn generate_operation_loop(
    first_wallet: EthereumWallet,
    total_new_wallets: i32,
    return_wallet: Option<EthereumWallet>,
    backup_dir: &Path,
) -> eyre::Result<TreeNode<Operation>> {
    if total_new_wallets < 0 {
        return Err(eyre::eyre!("total_new_wallets cannot be negative"));
    }

    // If no new wallets are needed, just create a single operation that returns to the return wallet
    if total_new_wallets == 0 {
        let operation = Operation {
            from: first_wallet.clone(),
            to: return_wallet.unwrap_or(first_wallet),
            amount: None,
        };
        return Ok(TreeNode::new(operation));
    }

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

    // At this point, operations vector is guaranteed to be non-empty
    operations.push(Operation {
        from: operations
            .last()
            .expect("operations vector cannot be empty")
            .to
            .clone(),
        to: return_wallet.unwrap_or(first_wallet),
        amount: None,
    });

    // Create the root node with the first operation
    let mut root = TreeNode::new(operations[0].clone());
    let mut current_parent = &mut root;

    // Add each subsequent operation as a child of the previous operation
    for operation in operations.into_iter().skip(1) {
        let new_node = TreeNode::new(operation);
        TreeNode::add_child(current_parent, &new_node);
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

    let mut root = create_dummy_root(&first_wallet);
    let mut last_op_node = &mut root;

    // Create first layer operations (one for each loop)
    for loop_index in 0..total_loops {
        // Create and attach funding operation
        let (funding_op_node, new_wallet) =
            create_funding_operation(&first_wallet, amount_per_wallet, backup_dir).await?;
        TreeNode::add_child(last_op_node, &funding_op_node);
        last_op_node = last_op_node.children.last_mut().unwrap();

        // Calculate wallets for this loop and attach it
        let wallets_for_this_loop = if loop_index == 0 {
            base_wallets_per_loop + remainder
        } else {
            base_wallets_per_loop
        };

        attach_loop_to_parent(
            last_op_node,
            new_wallet,
            wallets_for_this_loop,
            first_wallet.clone(),
            backup_dir,
        )
        .await?;
    }

    Ok(root)
}

/// Generates a tree of operations where loops are balanced to end at approximately the same depth.
/// The first operations are sequential (funding from the same wallet) and each has a loop of
/// decreasing size attached to it, ensuring all paths complete in roughly the same number of steps.
///
/// # Arguments
/// * `first_wallet` - The first wallet to start the tree
/// * `total_new_wallets` - The total number of new wallets to create (first layer and all created by children loops)
/// * `total_loops` - The number of loops to create
/// * `amount_per_wallet` - The amount of ether to send to each new first layer wallet
///
/// # Returns
/// * `TreeNode<Operation>` - The root node of the built tree
pub async fn generate_balanced_split_loops(
    first_wallet: EthereumWallet,
    total_new_wallets: i32,
    total_loops: i32,
    amount_per_wallet: U256,
    backup_dir: &Path,
) -> eyre::Result<TreeNode<Operation>> {
    // First, calculate how many wallets we need for the funding operations
    let funding_wallets = total_loops;
    let remaining_wallets = total_new_wallets - funding_wallets;

    if remaining_wallets < total_loops {
        return Err(eyre::eyre!(
            "Not enough wallets to create balanced loops. Need at least {} wallets but got {}",
            total_loops * 2,
            total_new_wallets
        ));
    }

    // Calculate decreasing sequence of wallets per loop
    let mut wallets_per_loop = Vec::with_capacity(total_loops as usize);
    let mut remaining = remaining_wallets;

    // Calculate base unit ensuring at least 1 wallet per loop
    let base_unit = std::cmp::max(
        1,
        (2 * remaining_wallets) / (total_loops * (total_loops + 1)),
    );

    for i in 0..total_loops {
        let wallets = if i == total_loops - 1 {
            remaining.max(1) // Ensure last loop gets at least 1 wallet
        } else {
            std::cmp::max(1, std::cmp::min(base_unit * (total_loops - i), remaining))
        };
        wallets_per_loop.push(wallets);
        remaining -= wallets;
        if remaining <= 0 && i < total_loops - 1 {
            // If we run out of wallets but still have loops to create,
            // give 1 wallet to each remaining loop
            for _ in (i + 1)..total_loops {
                wallets_per_loop.push(1);
            }
            break;
        }
    }

    let mut root = create_dummy_root(&first_wallet);
    let mut last_op_node = &mut root;

    // Create the chain of funding operations
    for wallets_in_loop in wallets_per_loop {
        // Create and attach funding operation
        let (funding_op_node, new_wallet) =
            create_funding_operation(&first_wallet, amount_per_wallet, backup_dir).await?;
        TreeNode::add_child(last_op_node, &funding_op_node);
        last_op_node = last_op_node.children.last_mut().unwrap();

        // Create and attach the loop
        attach_loop_to_parent(
            last_op_node,
            new_wallet,
            wallets_in_loop,
            first_wallet.clone(),
            backup_dir,
        )
        .await?;
    }

    Ok(root)
}
