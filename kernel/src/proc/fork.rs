//! fork(): create a child process with deep-copy of writable pages.
//!
//! Phase 4 strategy: deep copy for correctness. COW optimization deferred.

use super::task::Task;
use crate::mm::pmap;
use crate::mm::vm::vm_map::MapPerm;
use crate::mm::vm::vm_object::OwnedPage;
use alloc::sync::Arc;
use hal_common::{VirtAddr, PAGE_SIZE};

/// Fork the parent task with deep-copy of all mapped writable pages.
pub fn fork(parent: &Arc<Task>) -> Arc<Task> {
    let child = Task::new(Arc::downgrade(parent));
    klog!(
        proc,
        debug,
        "fork parent={} child={}",
        parent.pid,
        child.pid
    );

    // Deep-copy strategy: create independent VmObjects (not shadows)
    {
        let parent_vm = parent.vm_map.lock();
        let child_vm = parent_vm.fork_deep_copy();
        *child.vm_map.lock() = child_vm;
    }

    // Deep-copy: copy all pages from parent's VmObjects into child
    deep_copy_pages(parent, &child);

    // Share sigcode trampoline page from parent (read-only, not in vm_map)
    {
        let parent_pmap = parent.pmap.lock();
        if let Some(sigcode_pa) = pmap::pmap_extract(&parent_pmap, VirtAddr::new(super::signal::SIGCODE_VA)) {
            let mut child_pmap = child.pmap.lock();
            let prot = crate::map_perm!(R, X, U);
            let _ = pmap::pmap_enter(&mut child_pmap, VirtAddr::new(super::signal::SIGCODE_VA), sigcode_pa, prot, false);
        }
    }

    // Fork fd table (Arc-shared OpenFile entries per POSIX)
    {
        let parent_fds = parent.fd_table.lock();
        *child.fd_table.lock() = parent_fds.fork();
    }
    {
        let parent_cwd = parent.cwd.lock().clone();
        *child.cwd.lock() = parent_cwd;
    }

    // Copy parent's trap frame (child resumes past the ecall).
    // Child gets return value 0 in a0 (fork convention).
    // TODO: consider implement several HAL setter methods in trapframe, and remember to replace it here.
    // TODO: like skip_an_instruction()
    {
        let mut child_tf = child.trap_frame.lock();
        *child_tf = *parent.trap_frame.lock();
        child_tf.set_ret_val(0);
        child_tf.advance_pc(); // skip ecall — parent's sepc is advanced by dispatcher, child's must be done here
    }

    // Copy brk
    child.brk.store(
        parent.brk.load(core::sync::atomic::Ordering::Relaxed),
        core::sync::atomic::Ordering::Relaxed,
    );

    // Clear pending signals in child (POSIX: pending signals not inherited).
    // TODO: pack as a setter method    // Initial signals state
    child.signals.pending.store(
        crate::proc::signal::SigSet::empty(),
        core::sync::atomic::Ordering::Relaxed,
    );

    // Copy signal actions from parent.
    {
        let parent_actions = parent.signals.actions.lock();
        *child.signals.actions.lock() = *parent_actions;
    }

    // Copy blocked mask from parent.
    //TODO: pack as a setter method in signal?
    child.signals.blocked.store(
        parent
            .signals
            .blocked
            .load(core::sync::atomic::Ordering::Relaxed),
        core::sync::atomic::Ordering::Relaxed,
    );

    // Register child in parent's children list
    parent.children.lock().push(Arc::clone(&child));

    child
}

/// Deep-copy all pages from parent into child.
///
/// For writable VMAs: walk the parent's **pmap** (not VmObject) to find every
/// mapped page, allocate a fresh frame, copy the data, insert into the child's
/// VmObject, and map in the child's pmap.  We scan the pmap because
/// `fault_in_page_async` maps file-backed pages into the pmap without inserting
/// them into the VmObject -- so the VmObject may be empty even though pages
/// are actually mapped.
///
/// For read-only VMAs: share physical pages via pmap (VmObject already
/// Arc::clone'd by fork_deep_copy, so demand faults will find pages there).
fn deep_copy_pages(parent: &Arc<Task>, child: &Arc<Task>) {
    let parent_vm = parent.vm_map.lock();
    let child_vm = child.vm_map.lock();
    let mut child_pmap = child.pmap.lock();
    let parent_pmap = parent.pmap.lock();

    for vma in parent_vm.iter() {
        let is_writable = vma.prot.contains(MapPerm::W);

        if is_writable {
            // Walk every page in the VMA range via parent's pmap to find
            // all mapped pages (both VmObject-backed and pmap-only).
            let mut va = vma.range.start.as_usize();
            while va < vma.range.end.as_usize() {
                if let Some(src_pa) = pmap::pmap_extract(&parent_pmap, VirtAddr::new(va)) {
                    if let Some(mut new_frame) = crate::mm::allocator::alloc_anon_sync() {
                        unsafe {
                            new_frame.as_bytes_mut().copy_from_slice(src_pa.page_align_down().as_slice());
                        }
                        if let Some(child_vma) = child_vm.find_area(VirtAddr::new(va)) {
                            let new_phys = new_frame.phys();
                            let obj_idx = child_vma.pindex_for(VirtAddr::new(va));
                            let mut child_obj = child_vma.object.write();
                            child_obj.insert_page(obj_idx, OwnedPage::new_anonymous(new_frame));
                            let _ = pmap::pmap_enter(&mut child_pmap, VirtAddr::new(va), new_phys, child_vma.prot, false);
                        }
                    }
                }
                va += PAGE_SIZE;
            }
        } else {
            // Read-only: share pages currently in pmap.
            // The VmObject is already Arc::clone'd by fork_deep_copy, so
            // demand faults will find pages in the shared object.
            let mut va = vma.range.start.as_usize();
            while va < vma.range.end.as_usize() {
                if let Some(pa) = pmap::pmap_extract(&parent_pmap, VirtAddr::new(va)) {
                    let _ = pmap::pmap_enter(&mut child_pmap, VirtAddr::new(va), pa, vma.prot, false);
                }
                va += PAGE_SIZE;
            }
        }
    }
}
