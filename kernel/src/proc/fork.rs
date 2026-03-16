//! fork(): create a child process.
//!
//! Default path uses COW shadow chains (O(VMAs) instead of O(pages)).
//! Legacy deep-copy path available behind `#[cfg(feature = "fork-hardcopy")]`.

use alloc::sync::Arc;
#[cfg(not(feature = "fork-hardcopy"))]
use alloc::vec::Vec;

#[cfg(not(feature = "fork-hardcopy"))]
use crate::mm::vm::{MapPerm, VmMapping};
use crate::{
    hal_common::{VirtAddr, PAGE_SIZE},
    mm::{pmap, pmap_enter, pmap_extract},
    proc::{map_sigcode_page, Task, SIGCODE_VA},
};

/// Fork the parent task, creating a child with COW (or deep-copy) VM.
pub fn fork(parent: &Arc<Task>) -> Arc<Task> {
    let child = Task::new(Arc::downgrade(parent));
    klog!(
        proc,
        debug,
        "fork parent={} child={}",
        parent.pid,
        child.pid
    );
    // VM copy strategy: COW shadow chains (default) or deep copy (feature flag).
    #[cfg(not(feature = "fork-hardcopy"))]
    cow_fork_vm(parent, &child);
    #[cfg(feature = "fork-hardcopy")]
    deep_copy_pages(parent, &child);

    // Ensure child has sigcode trampoline page (not tracked in vm_map).
    // Prefer sharing parent's mapped page; fallback to creating a new one.
    //
    // Lock ordering: parent_vm_map (L1) → parent_pmap (L2) → child_vm_map (L1) → child_pmap (L2).
    // TODO: This acquires parent_pmap BEFORE child_pmap, but cow_fork_vm
    // (below, line ~210) acquires child_pmap before parent_pmap.  The
    // inconsistency is safe TODAY because the child is brand-new and no
    // other thread can reference it yet, but this should be unified to
    // always acquire parent before child for robustness.
    {
        let parent_vm = parent.vm_map.read();
        let parent_pmap = parent_vm.pmap_lock();
        let child_vm = child.vm_map.read();
        let mut child_pmap = child_vm.pmap_lock();
        if let Some(sigcode_pa) = pmap_extract(&parent_pmap, VirtAddr::new(SIGCODE_VA)) {
            let prot = crate::map_perm!(R, X, U);
            let _ = pmap_enter(
                &mut child_pmap,
                VirtAddr::new(SIGCODE_VA),
                sigcode_pa,
                prot,
                false,
            );
        } else {
            map_sigcode_page(&mut child_pmap);
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
    child.signals.pending.store(
        crate::proc::SigSet::empty(),
        core::sync::atomic::Ordering::Relaxed,
    );

    // Copy signal actions from parent.
    {
        let parent_actions = parent.signals.actions.lock();
        *child.signals.actions.lock() = *parent_actions;
    }

    // Copy blocked mask from parent.
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

// ---------------------------------------------------------------------------
// COW fork (default)
// ---------------------------------------------------------------------------

/// COW fork: create shadow chains for writable VMAs, share read-only VMAs.
///
/// For each writable VMA, creates a shadow VmObject backed by the parent's
/// object. Both parent and child PTEs are downgraded to read-only so that
/// the first write triggers a COW fault.
#[cfg(not(feature = "fork-hardcopy"))]
fn cow_fork_vm(parent: &Arc<Task>, child: &Arc<Task>) {
    let mut parent_vm = parent.vm_map.write();
    let mut child_vm = child.vm_map.write();

    // Build fork plans first, then apply parent mutations and child inserts.
    let mut parent_rebinds: Vec<(u64, VmMapping)> = Vec::new();
    let mut child_entries: Vec<crate::mm::vm::VmMapEntry> = Vec::new();
    let mut pmap_ranges: Vec<(u64, u64, MapPerm, bool)> = Vec::new();

    for vma in parent_vm.iter() {
        let is_writable = vma.protection.contains(MapPerm::W);

        let (parent_new_mapping, child_new_mapping) = match &vma.mapping {
            VmMapping::Guard => (None, VmMapping::Guard),
            mapping => {
                let parent_obj = match mapping.object() {
                    Some(obj) => obj,
                    None => {
                        // Should not happen for non-Guard variants
                        continue;
                    },
                };

                if is_writable {
                    // Writable mapping: both parent and child must get their own
                    // shadows backed by the original object, otherwise whichever
                    // side writes first will leak modifications to the other side.
                    let size_bytes = (vma.end() - vma.start()) as usize;
                    let parent_obj_shadow =
                        crate::mm::vm::VmObject::new_shadow(Arc::clone(parent_obj), size_bytes);
                    let child_obj_shadow =
                        crate::mm::vm::VmObject::new_shadow(Arc::clone(parent_obj), size_bytes);

                    // Keep file-backed demand-fault behavior on shadow heads.
                    {
                        let parent_read = parent_obj.read();
                        if let Some(ref pager) = parent_read.pager {
                            if !pager.is_anon() {
                                parent_obj_shadow.write().pager = Some(Arc::clone(pager));
                                child_obj_shadow.write().pager = Some(Arc::clone(pager));
                            }
                        }
                    }
                    (
                        Some(mapping.with_object(parent_obj_shadow)),
                        mapping.with_object(child_obj_shadow),
                    )
                } else {
                    // Read-only: share the same VmObject (no shadow needed)
                    (None, mapping.with_object(Arc::clone(parent_obj)))
                }
            },
        };

        if let Some(new_mapping) = parent_new_mapping {
            parent_rebinds.push((vma.start(), new_mapping));
        };

        let child_vma = crate::mm::vm::VmMapEntry::new(
            vma.start(),
            vma.end(),
            child_new_mapping,
            vma.protection,
        );
        child_entries.push(child_vma);
        pmap_ranges.push((vma.start(), vma.end(), vma.protection, is_writable));
    }

    // Rebind parent VMAs to their new COW shadow heads.
    for (start, new_mapping) in parent_rebinds {
        if let Some(vma) = parent_vm.lookup_mut(start) {
            vma.mapping = new_mapping;
        }
    }

    // Install child VMAs.
    for entry in child_entries {
        child_vm.insert_entry(entry).unwrap();
    }

    // Phase 2: set up pmap mappings
    //
    // Lock ordering: child_pmap (L2) → parent_pmap (L2).
    // TODO: This acquires child_pmap BEFORE parent_pmap, but the sigcode
    // block above (line ~36) acquires parent_pmap before child_pmap.
    // Safe today because the child is brand-new (no concurrent access),
    // but should be unified to parent-before-child everywhere.
    let child_pmap_arc = child_vm.pmap.clone();
    let mut child_pmap = child_pmap_arc.lock();
    let parent_pmap_arc = parent_vm.pmap.clone();
    let mut parent_pmap = parent_pmap_arc.lock();

    for (start, end, prot, is_writable) in pmap_ranges {
        if is_writable && !prot.contains(MapPerm::W) {
            continue;
        }
        if prot.is_empty() {
            continue;
        }

        let mut va = start as usize;
        while va < end as usize {
            if let Some(pa) = pmap::pmap_extract(&parent_pmap, VirtAddr::new(va)) {
                if is_writable {
                    // Strip W from parent PTE to force COW fault on parent writes too
                    let ro_prot = prot & !MapPerm::W;
                    pmap::pmap_protect(
                        &mut parent_pmap,
                        VirtAddr::new(va),
                        VirtAddr(va + PAGE_SIZE),
                        ro_prot,
                    );
                    // Map same physical page read-only in child
                    let _ =
                        pmap::pmap_enter(&mut child_pmap, VirtAddr::new(va), pa, ro_prot, false);
                } else {
                    // Read-only: share with same permissions
                    let _ = pmap::pmap_enter(&mut child_pmap, VirtAddr::new(va), pa, prot, false);
                }
            }
            va += PAGE_SIZE;
        }
    }
}

// ---------------------------------------------------------------------------
// Deep-copy fork (legacy, feature-gated)
// ---------------------------------------------------------------------------

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
/// Should be used as a fallback when COW errors.
#[cfg(feature = "fork-hardcopy")]
fn deep_copy_pages(parent: &Arc<Task>, child: &Arc<Task>) {
    let parent_vm = parent.vm_map.read();
    let mut child_vm = child.vm_map.write();

    // Copy VmMapEntries
    for vma in parent_vm.iter() {
        let new_mapping = match &vma.mapping {
            crate::mm::vm::VmMapping::Guard => crate::mm::vm::VmMapping::Guard,
            mapping => {
                let parent_obj = mapping.object().expect("non-Guard must have object");
                let size = (vma.end() - vma.start()) as usize;
                // Create a clone of the object for the child (not a shadow)
                let new_obj = crate::mm::vm::VmObject::new_anon(size);
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
                mapping.with_object(new_obj)
            },
        };
        let child_vma =
            crate::mm::vm::VmMapEntry::new(vma.start(), vma.end(), new_mapping, vma.protection);
        child_vm.insert_entry(child_vma).unwrap();
    }

    let child_pmap_arc = child_vm.pmap.clone();
    let mut child_pmap = child_pmap_arc.lock();
    let parent_pmap_arc = parent_vm.pmap.clone();
    let parent_pmap = parent_pmap_arc.lock();

    for vma in parent_vm.iter() {
        let is_writable = vma.protection.contains(crate::mm::vm::MapPerm::W);

        if is_writable {
            // Walk every page in the VMA range via parent's pmap to find
            // all mapped pages (both VmObject-backed and pmap-only).
            let mut va = vma.start() as usize;
            while va < vma.end() as usize {
                if let Some(src_pa) = pmap::pmap_extract(&parent_pmap, VirtAddr::new(va)) {
                    if let Some(new_frame) = crate::mm::alloc_anon_sync() {
                        new_frame.as_page_slice_mut().copy_from_slice(
                            src_pa.page_align_down().into_kernel_vaddr().as_page_slice(),
                        );

                        if let Some(child_vma) = child_vm.lookup_mut(va as u64) {
                            if let Some(object) = child_vma.mapping.object() {
                                let new_phys = new_frame.phys();
                                let offset = child_vma.mapping.offset();
                                let obj_idx =
                                    (offset + (va as u64 - child_vma.start())) / PAGE_SIZE as u64;
                                let mut child_obj = object.write();
                                child_obj.insert_page(
                                    crate::hal_common::PageNum::new(obj_idx as usize),
                                    crate::mm::vm::PageRef::new(new_phys),
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
            let mut va = vma.start() as usize;
            while va < vma.end() as usize {
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
