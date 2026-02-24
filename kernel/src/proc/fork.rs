//! fork(): create a child process with COW VmMap.
//!
//! Implemented in Task 2.

use alloc::sync::Arc;
use super::task::Task;
use crate::mm::pmap;
use crate::mm::vm::vm_map::{MapPerm, VmAreaType};

/// Fork the parent task, creating a child with COW shadow chains.
pub fn fork(parent: &Arc<Task>) -> Arc<Task> {
    let child = Task::new(Arc::downgrade(parent));

    // COW: fork the parent's VmMap (inserts shadow objects)
    {
        let parent_vm = parent.vm_map.lock();
        let child_vm = parent_vm.fork();

        // Write-protect parent's writable anonymous PTEs for COW.
        // Child starts with empty pmap — pages will be demand-faulted.
        {
            let mut parent_pmap = parent.pmap.lock();
            for vma in parent_vm.iter() {
                if vma.vma_type == VmAreaType::Anonymous && vma.prot.contains(MapPerm::W) {
                    let ro_prot = vma.prot & !MapPerm::W;
                    pmap::pmap_protect(
                        &mut parent_pmap,
                        vma.range.start,
                        vma.range.end,
                        ro_prot,
                    );
                }
            }
        }

        *child.vm_map.lock() = child_vm;
    }

    // Fork fd table (Arc-shared OpenFile entries per POSIX)
    {
        let parent_fds = parent.fd_table.lock();
        *child.fd_table.lock() = parent_fds.fork();
    }

    // Copy parent's trap frame (child resumes at same PC).
    // Child gets return value 0 in a0 (fork convention).
    {
        let mut child_tf = child.trap_frame.lock();
        *child_tf = *parent.trap_frame.lock();
        child_tf.set_ret_val(0);
    }

    // Copy brk
    child.brk.store(
        parent.brk.load(core::sync::atomic::Ordering::Relaxed),
        core::sync::atomic::Ordering::Relaxed,
    );

    // Register child in parent's children list
    parent.children.lock().push(Arc::clone(&child));

    child
}
