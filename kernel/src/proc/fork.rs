//! Legacy deep-copy fork path (feature-gated).
//!
//! The default COW fork logic now lives in `VmMap::cow_fork_into()`.
//! This module only retains the `deep_copy_pages` fallback behind
//! `#[cfg(feature = "fork-hardcopy")]`.

#[cfg(feature = "fork-hardcopy")]
use alloc::sync::Arc;

#[cfg(feature = "fork-hardcopy")]
use crate::{
    hal_common::{VirtAddr, PAGE_SIZE},
    mm::pmap,
    proc::Task,
};

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
pub(super) fn deep_copy_pages(parent: &Arc<Task>, child: &Arc<Task>) {
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
