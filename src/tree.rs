//! Tree data structure for organizing dependent operations.
//!
//! This module provides a tree structure that maintains parent-child relationships
//! between operations, ensuring proper execution order of dependent operations.

use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// A tree node that can store any value type and maintain parent-child relationships.
/// Used to organize operations in a dependency tree where children depend on their parent's completion.
#[derive(Debug, Clone)]
pub struct TreeNode<T> {
    /// Unique identifier for this node
    pub id: usize,
    /// The value stored in this node
    pub value: T,
    /// Child nodes that depend on this node's operation
    pub children: Vec<TreeNode<T>>,
}

impl<T: Clone + Debug> TreeNode<T> {
    /// Creates a new tree node with the given value and no children.
    ///
    /// # Arguments
    /// * `value` - The value to store in the new node
    ///
    /// # Returns
    /// A new tree node with the given value and no children
    pub fn new(value: T) -> TreeNode<T> {
        TreeNode {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            value,
            children: Vec::new(),
        }
    }

    /// Adds a child node to the parent node, establishing a dependency relationship.
    ///
    /// # Arguments
    /// * `parent` - The parent node to add the child to
    /// * `child` - The child node to add
    pub fn add_child(parent: &mut TreeNode<T>, child: &TreeNode<T>) {
        parent.children.push(child.clone());
    }

    /// Flattens the tree into a vector where parent operations precede their children.
    /// This ensures that operations are executed in the correct dependency order.
    ///
    /// # Returns
    /// * `Vec<T>` - Flattened list of operations in dependency order
    pub fn flatten(&self) -> Vec<T> {
        let mut operations_list = vec![];
        operations_list.push(self.value.clone());
        for child in self.children.iter() {
            operations_list.extend(child.flatten());
        }
        operations_list
    }

    /// Recursively searches for a node with the given ID.
    /// Returns None if no node with the ID is found.
    ///
    /// # Arguments
    /// * `id` - The ID of the node to find
    ///
    /// # Returns
    /// * `Option<&TreeNode<T>>` - The node with the given ID, if found
    pub fn find_node_by_id(&self, id: usize) -> Option<&TreeNode<T>> {
        if self.id == id {
            return Some(self);
        }

        for child in &self.children {
            if let Some(found) = child.find_node_by_id(id) {
                return Some(found);
            }
        }

        None
    }
}
