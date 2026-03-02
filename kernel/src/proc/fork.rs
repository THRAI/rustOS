//! fork(): create a child process with deep-copy of writable pages.
//!
//! Phase 4 strategy: deep copy for correctness. COW optimization deferred.

use alloc::sync::Arc;
use super::task::Task;
use crate::mm::pmap;
use crate::mm::vm::vm_map::{MapPerm, VmAreaType};
use crate::mm::vm::vm_object::OwnedPage;
use hal_common::{PhysAddr, VirtAddr, PAGE_SIZE};

/// Fork the parent task with deep-copy of all mapped writable pages.
pub fn fork(parent: &Arc<Task>) -> Arc<Task> {
    let child = Task::new(Arc::downgrade(parent));
    klog!(proc, debug, "fork parent={} child={}", parent.pid, child.pid);

    // 1. Fork VmMap structure (shadow objects for anon, shared for file-backed)
    // 2. Deep-copy all pages the parent has mapped in its pmap
    {
        let parent_vm = parent.vm_map.lock();
        let child_vm = parent_vm.fork();
        *child.vm_map.lock() = child_vm;
    }

    // Deep-copy: walk parent's pmap, copy mapped pages into child
    deep_copy_pages(parent, &child);

    // Map sigcode trampoline page in child (not in vm_map, so deep_copy misses it)
    {
        let mut child_pmap = child.pmap.lock();
        super::signal::map_sigcode_page(&mut child_pmap);
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
    {
        let mut child_tf = child.trap_frame.lock();
        *child_tf = *parent.trap_frame.lock();
        child_tf.set_ret_val(0);
        child_tf.sepc += 4; // skip ecall — parent's sepc is advanced by dispatcher, child's must be done here
    }

    // Copy brk
    child.brk.store(
        parent.brk.load(core::sync::atomic::Ordering::Relaxed),
        core::sync::atomic::Ordering::Relaxed,
    );

    // Clear pending signals in child (POSIX: pending signals not inherited).
    child.signals.pending.store(0, core::sync::atomic::Ordering::Relaxed);
    // Copy signal actions from parent.
    {
        let parent_actions = parent.signals.actions.lock();
        *child.signals.actions.lock() = *parent_actions;
    }
    // Copy blocked mask from parent.
    child.signals.blocked.store(
        parent.signals.blocked.load(core::sync::atomic::Ordering::Relaxed),
        core::sync::atomic::Ordering::Relaxed,
    );

    // Register child in parent's children list
    parent.children.lock().push(Arc::clone(&child));

    child
}

/// Deep-copy all pages mapped in the parent's pmap into the child.
///
/// For writable VMAs: allocate new frame, memcpy, map in child pmap + insert
/// into child VmObject.
/// For read-only VMAs: share the same physical page (map directly).
fn deep_copy_pages(parent: &Arc<Task>, child: &Arc<Task>) {
    let parent_vm = parent.vm_map.lock();
    let parent_pmap = parent.pmap.lock();
    let child_vm = child.vm_map.lock();
    let mut child_pmap = child.pmap.lock();

    for vma in parent_vm.iter() {
        let mut va = vma.range.start.as_usize();
        let end = vma.range.end.as_usize();

        while va < end {
            let va_virt = VirtAddr::new(va);
            if let Some(parent_pa) = pmap::pmap_extract(&parent_pmap, va_virt) {
                let is_writable = vma.prot.contains(MapPerm::W);

                if is_writable {
                    // Deep copy: new frame + memcpy
                    if let Some(new_frame) = crate::mm::allocator::frame_alloc_sync() {
                        unsafe {
                            let src = parent_pa.as_usize() as *const u8;
                            let dst = new_frame.as_usize() as *mut u8;
                            core::ptr::copy_nonoverlapping(src, dst, PAGE_SIZE);
                        }
                        // Insert into child's VmObject
                        if let Some(child_vma) = child_vm.find_area(va_virt) {
                            let obj_offset = ((va - child_vma.range.start.as_usize()) / PAGE_SIZE) as u64
                                + child_vma.obj_offset;
                            let mut obj = child_vma.object.write();
                            obj.insert_page(obj_offset, OwnedPage::new_anonymous(new_frame));
                        }
                        let _ = pmap::pmap_enter(&mut child_pmap, va_virt, new_frame, vma.prot, false);
                    }
                } else {
                    // Read-only: share physical page
                    let _ = pmap::pmap_enter(&mut child_pmap, va_virt, parent_pa, vma.prot, false);
                }
            }
            va += PAGE_SIZE;
        }
    }
}
