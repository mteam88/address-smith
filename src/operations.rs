use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;
use alloy_primitives::{Address, U256};
use std::path::Path;

use crate::tree::TreeNode;
use crate::types::Operation;
use crate::utils::generate_signer;

/// Creates a root node with a dummy operation from a wallet to itself
fn create_dummy_root(address: &Address) -> TreeNode<Operation> {
    TreeNode::new(Operation {
        from: *address,
        to: *address,
        amount: None,
    })
}

/// Creates a funding operation node that sends funds from the first wallet to a new wallet
async fn create_funding_operation(
    first_address: &Address,
    amount: U256,
    backup_dir: &Path,
) -> eyre::Result<(TreeNode<Operation>, PrivateKeySigner)> {
    let new_signer = generate_signer(backup_dir).await?;
    let operation = Operation {
        from: *first_address,
        to: new_signer.address(),
        amount: Some(amount),
    };
    Ok((TreeNode::new(operation), new_signer))
}

/// Creates a loop starting from a wallet and adds it as a child of the given parent node
async fn attach_loop_to_parent(
    parent: &mut TreeNode<Operation>,
    start_address: Address,
    addresses_in_loop: i32,
    return_address: Address,
    backup_dir: &Path,
) -> eyre::Result<Vec<PrivateKeySigner>> {
    let (loop_root, signers) = generate_operation_loop(
        start_address,
        addresses_in_loop,
        Some(return_address),
        backup_dir,
    )
    .await?;
    TreeNode::add_child(parent, &loop_root);
    Ok(signers)
}

/// Generates a sequence of operations forming a loop starting and ending with the first wallet.
/// Creates a chain where each operation transfers funds to a new wallet, ultimately returning to the first.
/// If a return wallet is provided, the last operation will return to it instead of the first wallet.
pub async fn generate_operation_loop(
    first_address: Address,
    total_new_addresses: i32,
    return_address: Option<Address>,
    backup_dir: &Path,
) -> eyre::Result<(TreeNode<Operation>, Vec<PrivateKeySigner>)> {
    if total_new_addresses < 0 {
        return Err(eyre::eyre!("total_new_addresses cannot be negative"));
    }

    // If no new addresses are needed, just create a single operation that returns to the return wallet
    if total_new_addresses == 0 {
        let operation = Operation {
            from: first_address,
            to: return_address.unwrap_or(first_address),
            amount: None,
        };
        return Ok((TreeNode::new(operation), vec![]));
    }

    let mut operations = vec![];
    let mut current_address = first_address;

    // Create chain of operations through new addresses
    let mut signers = vec![];
    for _ in 0..(total_new_addresses - 1) {
        let next_signer = generate_signer(backup_dir).await?;
        let operation = Operation {
            from: current_address,
            to: next_signer.address(),
            amount: None,
        };
        operations.push(operation);
        current_address = next_signer.address();
        signers.push(next_signer);
    }

    // Create final operation back to return wallet
    operations.push(Operation {
        from: current_address,
        to: return_address.unwrap_or(first_address),
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

    Ok((root, signers))
}

/// Generates a tree of operations where loops are balanced to end at approximately the same depth.
/// The first operations are sequential (funding from the same wallet) and each has a loop of
/// decreasing size attached to it, ensuring all paths complete in roughly the same number of steps.
///
/// # Arguments
/// * `first_wallet` - The first wallet to start the tree
/// * `total_new_addresses` - The total number of new addresses to create (first layer and all created by children loops)
/// * `total_loops` - The number of loops to create
/// * `amount_per_wallet` - The amount of ether to send to each new first layer wallet
///
/// # Returns
/// * `TreeNode<Operation>` - The root node of the built tree
pub async fn generate_balanced_split_loops(
    root_wallet: &mut EthereumWallet,
    first_address: Address,
    total_new_addresses: i32,
    max_total_loops: i32,
    amount_per_wallet: U256,
    backup_dir: &Path,
) -> eyre::Result<TreeNode<Operation>> {
    let addresses_per_loop = calculate_addresses_per_loop(total_new_addresses, max_total_loops);

    let mut root = create_dummy_root(&first_address);
    let mut last_op_node = &mut root;

    // Create the chain of funding operations
    for addresses_in_loop in addresses_per_loop {
        // Skip if the loop is empty
        if addresses_in_loop == 0 {
            continue;
        }

        // Create and attach funding operation
        let (funding_op_node, new_signer) =
            create_funding_operation(&first_address, amount_per_wallet, backup_dir).await?;
        root_wallet.register_signer(new_signer.clone());

        TreeNode::add_child(last_op_node, &funding_op_node);
        last_op_node = last_op_node.children.last_mut().unwrap();

        // Create and attach the loop
        let signers = attach_loop_to_parent(
            last_op_node,
            new_signer.address(),
            addresses_in_loop,
            first_address,
            backup_dir,
        )
        .await?;

        for signer in signers {
            root_wallet.register_signer(signer.clone());
        }
    }

    Ok(root)
}

/// Distributes `total_new_addresses` across up to `max_total_loops` loops
/// so that the overall completion time (funding offset + tasks per loop)
/// is minimized. Returns a Vec where each element is the number of tasks
/// allocated to that loop in order.
fn calculate_addresses_per_loop(total_new_addresses: i32, max_total_loops: i32) -> Vec<i32> {
    // Edge cases
    if total_new_addresses <= 0 || max_total_loops <= 0 {
        return vec![];
    }

    // 1. Define a search range [left, right].
    // An upper bound for the time could be total_new_addresses + max_total_loops
    // (worst case: all tasks to 1 loop, with the rest loops "wasted", plus offsets).
    let mut left = 0;
    let mut right = total_new_addresses + max_total_loops; // safe big upper bound

    // 2. Binary search for the minimal feasible T.
    while left < right {
        let mid = (left + right) / 2;

        // Calculate capacity if each loop i can take up to (mid - i) tasks,
        // for i in 0..(loop_count - 1), limited by i < mid (since mid - i <= 0 otherwise).
        let used_loops = std::cmp::min(max_total_loops, mid);
        let mut capacity: i64 = 0; // might exceed i32 during sum
        for i in 0..used_loops {
            let cap = mid - i;
            if cap > 0 {
                capacity += cap as i64;
            } else {
                break;
            }
        }

        // Compare the capacity to total_new_addresses
        if capacity >= total_new_addresses as i64 {
            // mid is large enough, try to reduce the upper bound
            right = mid;
        } else {
            // mid is not large enough
            left = mid + 1;
        }
    }

    let min_time = left;

    // 3. Assign tasks to each loop in order
    let mut result = Vec::new();
    let mut remaining = total_new_addresses;
    for i in 0..max_total_loops {
        // if offset i is already >= min_time, (min_time - i) would be <= 0
        if i >= min_time {
            break;
        }
        let can_assign = min_time - i;
        let assign = can_assign.min(remaining);
        result.push(assign);
        remaining -= assign;
        if remaining <= 0 {
            break;
        }
    }

    result
}
