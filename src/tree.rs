use std::{fmt::Debug, sync::{Arc, Mutex}};

#[derive(Debug)]
pub struct TreeNode<T> {
    pub value: T,
    pub children: Vec<Arc<Mutex<TreeNode<T>>>>,
}

impl<T: Clone + Debug> TreeNode<T> {
    pub fn new(value: T) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(TreeNode {
            value,
            children: Vec::new(),
        }))
    }

    pub fn add_child(parent: Arc<Mutex<Self>>, child: Arc<Mutex<TreeNode<T>>>) {
        parent.lock().unwrap().children.push(child);
    }

    /// Flattens the tree so that any parent operation is executed before any of it's children.
    pub fn flatten(&self) -> Vec<T> {
        let mut operations_list = vec![];
        operations_list.push(self.value.clone());
        for child in self.children.iter() {
            operations_list.extend(child.lock().unwrap().flatten());
        }
        operations_list
    }
} 