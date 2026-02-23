//! fork(): create a child process with COW VmMap.
//!
//! Implemented in Task 2.

use alloc::sync::Arc;
use super::task::Task;

/// Fork the parent task, creating a child with COW shadow chains.
pub fn fork(parent: &Arc<Task>) -> Arc<Task> {
    let child = Task::new(Arc::downgrade(parent));

    // COW: fork the parent's VmMap (inserts shadow objects)
    {
        let parent_vm = parent.vm_map.lock();
        let child_vm = parent_vm.fork();
        *child.vm_map.lock() = child_vm;
    }

    // Fork fd table (Arc-shared OpenFile entries per POSIX)
    {
        let parent_fds = parent.fd_table.lock();
        *child.fd_table.lock() = parent_fds.fork();
    }

    // Register child in parent's children list
    parent.children.lock().push(Arc::clone(&child));

    child
}
