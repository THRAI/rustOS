//! fork(): create a child process with deep-copy of writable pages.
//!
//! Phase 4 strategy: deep copy for correctness. COW optimization deferred.

use super::task::Task;
use crate::mm::pmap;
use alloc::sync::Arc;
use crate::hal_common::{VirtAddr, PAGE_SIZE};

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
    // Deep-copy: copy all pages from parent's VmObjects into child
    deep_copy_pages(parent, &child);

    // Ensure child has sigcode trampoline page (not tracked in vm_map).
    // Prefer sharing parent's mapped page; fallback to creating a new one.
    {
        let parent_vm = parent.vm_map.lock();
        let parent_pmap = parent_vm.pmap_lock();
        let child_vm = child.vm_map.lock();
        let mut child_pmap = child_vm.pmap_lock();
        if let Some(sigcode_pa) =
            crate::mm::pmap::pmap_extract(&parent_pmap, VirtAddr::new(super::signal::SIGCODE_VA))
        {
            let prot = crate::map_perm!(R, X, U);
            let _ = crate::mm::pmap::pmap_enter(
                &mut child_pmap,
                VirtAddr::new(super::signal::SIGCODE_VA),
                sigcode_pa,
                prot,
                false,
            );
        } else {
            super::signal::map_sigcode_page(&mut child_pmap);
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
    let mut child_vm = child.vm_map.lock();

    // Copy VmMapEntries
    for vma in parent_vm.iter() {
        let new_store = match &vma.store {
            crate::mm::vm::map::entry::BackingStore::Object { offset, object: parent_obj } => {
                let size = vma.end - vma.start;
                // Create a clone of the object for the child (not a shadow)
                let new_obj = crate::mm::vm::vm_object::VmObject::new_anon(size as usize / PAGE_SIZE);
                // Copy pager from parent so child can demand-fault file-backed pages
                // that the parent hasn't touched yet. Without this, unfaulted pages
                // get zero-filled (AnonPager) instead of loaded from the ELF vnode.
                {
                    let parent_read = parent_obj.read();
                    if let Some(ref pager) = parent_read.pager {
                        if !pager.is_anon() {
                            new_obj.write().pager = Some(Arc::clone(pager));
                        }
                    }
                }
                crate::mm::vm::map::entry::BackingStore::Object {
                    object: new_obj,
                    offset: *offset,
                }
            }
            crate::mm::vm::map::entry::BackingStore::SubMap { .. } => {
                panic!("SubMap not supported in fork");
            }
            crate::mm::vm::map::entry::BackingStore::Guard => {
                crate::mm::vm::map::entry::BackingStore::Guard
            }
        };
        let child_vma = crate::mm::vm::map::entry::VmMapEntry::new(
            vma.start,
            vma.end,
            new_store,
            vma.flags,
            vma.protection,
        );
        child_vm.insert_entry(child_vma).unwrap();
    }

    let child_pmap_arc = child_vm.pmap.clone();
    let mut child_pmap = child_pmap_arc.lock();
    let parent_pmap_arc = parent_vm.pmap.clone();
    let parent_pmap = parent_pmap_arc.lock();

    for vma in parent_vm.iter() {
        let is_writable = vma
            .protection
            .contains(crate::mm::vm::map::entry::MapPerm::W);

        if is_writable {
            // Walk every page in the VMA range via parent's pmap to find
            // all mapped pages (both VmObject-backed and pmap-only).
            let mut va = vma.start as usize;
            while va < vma.end as usize {
                if let Some(src_pa) = pmap::pmap_extract(&parent_pmap, VirtAddr::new(va)) {
                    if let Some(new_frame) = crate::mm::allocator::alloc_anon_sync() {
                        unsafe {
                            new_frame
                                .as_bytes_mut()
                                .copy_from_slice(src_pa.page_align_down().into_kernel_vaddr().as_page_slice());
                        }
                        if let Some(child_vma) = child_vm.lookup_mut(va as u64) {
                            if let crate::mm::vm::map::entry::BackingStore::Object {
                                object,
                                offset,
                            } = &child_vma.store
                            {
                                let new_phys = new_frame.phys();
                                let obj_idx =
                                    (va as u64 - child_vma.start) / PAGE_SIZE as u64 + offset;
                                let mut child_obj = object.write();
                                let mut page = crate::mm::vm::page::VmPage::new();
                                page.phys_addr = new_phys;
                                child_obj.insert_page(
                                    crate::hal_common::addr::VirtPageNum(obj_idx as usize),
                                    Arc::new(page),
                                );
                                let _ = pmap::pmap_enter(
                                    &mut child_pmap,
                                    VirtAddr::new(va),
                                    new_phys,
                                    child_vma.protection,
                                    false,
                                );
                            }
                        }
                    }
                }
                va += PAGE_SIZE;
            }
        } else {
            // Read-only: share pages currently in pmap.
            // The VmObject is already Arc::clone'd by fork_deep_copy, so
            // demand faults will find pages in the shared object.
            let mut va = vma.start as usize;
            while va < vma.end as usize {
                if let Some(pa) = pmap::pmap_extract(&parent_pmap, VirtAddr::new(va)) {
                    let _ = pmap::pmap_enter(
                        &mut child_pmap,
                        VirtAddr::new(va),
                        pa,
                        vma.protection,
                        false,
                    );
                }
                va += PAGE_SIZE;
            }
        }
    }
}
