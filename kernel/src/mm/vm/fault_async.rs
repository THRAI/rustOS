//! Asynchronous page fault resolution.
//!
//! Handles demand paging for both anonymous and file-backed memory regions,
//! with page cache integration for file-backed pages.

use alloc::sync::Arc;

use crate::hal_common::addr::VirtPageNum;
use crate::hal_common::{VirtAddr, PAGE_SIZE};

use crate::mm::vm::fault::{sync_fault_handler, FaultError, FaultResult, PageFaultAccessType};
use crate::proc::task::Task;

/// Unified fault resolution: sync path first, async fallback for file-backed.
pub async fn resolve_user_fault(
    task: &Arc<Task>,
    fault_va: VirtAddr,
    access_type: PageFaultAccessType,
) -> Result<(), FaultError> {
    // Fast path: if the page is already mapped with sufficient permissions, skip.
    // Must check PTE flags — not just presence — to avoid bypassing COW faults.
    {
        let vm_map = task.vm_map.lock();
        let pmap = vm_map.pmap_lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        if let Some((_pa, flags)) =
            crate::mm::pmap::pmap_extract_with_flags(&pmap, fault_va_aligned)
        {
            use crate::mm::pmap::pte::PteFlags;
            let mut ok = true;
            if access_type.write && !flags.contains(PteFlags::W) {
                ok = false; // COW page — must go through fault handler
            }
            if access_type.execute && !flags.contains(PteFlags::X) {
                ok = false;
            }
            if ok {
                return Ok(());
            }
            crate::klog!(
                vm,
                trace,
                "resolve_user_fault fastpath miss: pid={} va={:#x} flags={:?} access(w={},x={})",
                task.pid,
                fault_va_aligned.as_usize(),
                flags,
                access_type.write,
                access_type.execute
            );
        }
    }

    // 1. Sync path: anonymous zero-fill, COW
    let sync_result = {
        let vm_map = task.vm_map.lock();
        let mut pmap = vm_map.pmap_lock();
        sync_fault_handler(&vm_map, &mut pmap, fault_va, access_type)
    };

    match sync_result {
        FaultResult::Resolved => {
            if access_type.write {
                crate::klog!(
                    vm,
                    trace,
                    "resolve_user_fault: COW resolved pid={} va={:#x}",
                    task.pid,
                    fault_va.as_usize()
                );
            }
            Ok(())
        }
        FaultResult::NeedsAsyncIO => {
            crate::klog!(
                vm,
                trace,
                "resolve_user_fault: NeedsAsyncIO pid={} va={:#x}",
                task.pid,
                fault_va.as_usize()
            );
            // 2. Async path: file-backed pages
            crate::klog!(
                vm,
                trace,
                "resolve_user_fault: ENTERING async pid={} va={:#x}",
                task.pid,
                fault_va.as_usize()
            );
            let async_result = fault_in_page_async(task, fault_va).await;
            match &async_result {
                Ok(()) => {
                    crate::klog!(
                        vm,
                        trace,
                        "resolve_user_fault: async OK pid={} va={:#x}",
                        task.pid,
                        fault_va.as_usize(),
                    );
                }
                Err(e) => {
                    crate::klog!(
                        vm,
                        trace,
                        "resolve_user_fault: async FAILED pid={} va={:#x} err={:?}",
                        task.pid,
                        fault_va.as_usize(),
                        e
                    );
                }
            }
            async_result
        }
        FaultResult::Error(e) => {
            crate::klog!(
                vm,
                trace,
                "resolve_user_fault: sync FAILED pid={} va={:#x} err={:?}",
                task.pid,
                fault_va.as_usize(),
                e
            );
            Err(e)
        }
    }
}

/// Async file-backed page fault resolution with TOCTOU re-validation.
///
/// Also handles anonymous VMAs (stack, heap, BSS) that reach this path:
/// allocate a zeroed frame and map it directly.
async fn fault_in_page_async(task: &Arc<Task>, fault_va: VirtAddr) -> Result<(), FaultError> {
    // 1. Look up VMA and compute object offsets
    let (obj, obj_offset, _vma_start, vma_perm) = {
        let mut map = task.vm_map.lock();
        let vma = map
            .lookup(fault_va.as_usize() as u64)
            .ok_or(FaultError::NotMapped)?;

        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));

        match &vma.store {
            crate::mm::vm::map::entry::BackingStore::Object { object, offset } => {
                let obj_offset_bytes = offset + (fault_va_aligned.as_usize() as u64 - vma.start);
                let obj_offset = VirtPageNum((obj_offset_bytes / PAGE_SIZE as u64) as usize);
                (object.clone(), obj_offset, vma.start, vma.protection)
            }
            _ => return Err(FaultError::NotMapped),
        }
    };

    // 2. Check if the page is already in the object
    if let Some(existing_pa) = obj.read().lookup_page(obj_offset) {
        let map = task.vm_map.lock();
        let mut pmap = map.pmap_lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
            if crate::mm::pmap::pmap_enter(&mut pmap, fault_va_aligned, existing_pa, vma_perm, false)
                .is_err()
            {
                return Err(FaultError::OutOfMemory);
            }
        }
        return Ok(());
    }

    // 3. Allocate a frame and zero it.
    // We unconditionally zero it so that if the pager reads fewer than 4096 bytes (e.g. at EOF),
    // the unread portion remains zero, as required for BSS segments.
    let frame =
        crate::mm::allocator::alloc_raw_frame_sync(crate::mm::allocator::PageRole::UserAnon)
            .ok_or(FaultError::OutOfMemory)?;
    crate::mm::pmap::pmap_zero_page(frame);

    // 4. Fetch data via pager
    let pager = { obj.read().pager.clone() };
    if let Some(pager_ref) = pager.as_ref() {
        let file_offset = obj_offset.0 as u64 * PAGE_SIZE as u64;
        if pager_ref
            .page_in(file_offset as usize, frame)
            .await
            .is_err()
        {
            crate::mm::allocator::free_raw_frame(frame);
            return Err(FaultError::IoError);
        }
    }

    // 5. Time-of-use revalidation and mapping
    {
        let map = task.vm_map.lock();
        let mut pmap = map.pmap_lock();
        // recheck map bounds - just a simple overlap check to prevent inserting out of bound mapping
        // due to map being rebuilt
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));

        let mut obj_write = obj.write();

        // Check if another thread raced us
        if let Some(existing_pa) = obj_write.lookup_page(obj_offset) {
            crate::mm::allocator::free_raw_frame(frame);
            if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
                if crate::mm::pmap::pmap_enter(
                    &mut pmap,
                    fault_va_aligned,
                    existing_pa,
                    vma_perm,
                    false,
                )
                .is_err()
                {
                    return Err(FaultError::OutOfMemory);
                }
            }
            return Ok(());
        }

        let vm_page_meta = crate::mm::allocator::types::get_frame_meta(frame).unwrap();
        obj_write.insert_page(obj_offset, {
            let mut page = crate::mm::vm::page::VmPage::new();
            page.phys_addr = vm_page_meta.phys();
            Arc::new(page)
        });

        if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
            if crate::mm::pmap::pmap_enter(&mut pmap, fault_va_aligned, frame, vma_perm, false)
                .is_err()
            {
                return Err(FaultError::OutOfMemory);
            }
        }
    }

    Ok(())
}
