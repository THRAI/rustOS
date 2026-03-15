//! Asynchronous page fault resolution.
//!
//! Handles demand paging for both anonymous and file-backed memory regions,
//! with page cache integration for file-backed pages.

use alloc::sync::Arc;

use crate::{
    hal_common::{Errno, VirtAddr, PAGE_SIZE},
    mm::{
        pmap_enter, pmap_extract, pmap_extract_with_flags, pmap_zero_page,
        vm::{page_ref::PageRef, sync_fault_handler, FaultResult, PageFaultAccessType},
        PteFlags,
    },
    proc::Task,
};

/// Unified fault resolution: sync path first, async fallback for file-backed.
pub async fn resolve_user_fault(
    task: &Arc<Task>,
    fault_va: VirtAddr,
    access_type: PageFaultAccessType,
) -> Result<(), Errno> {
    // Fast path: if the page is already mapped with sufficient permissions, skip.
    // Must check PTE flags — not just presence — to avoid bypassing COW faults.
    {
        let vm_map = task.vm_map.lock();
        let pmap = vm_map.pmap_lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        if let Some((_pa, flags)) = pmap_extract_with_flags(&pmap, fault_va_aligned) {
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
        },
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
                },
                Err(e) => {
                    crate::klog!(
                        vm,
                        trace,
                        "resolve_user_fault: async FAILED pid={} va={:#x} err={:?}",
                        task.pid,
                        fault_va.as_usize(),
                        e
                    );
                },
            }
            async_result
        },
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
        },
    }
}

/// Async file-backed page fault resolution with TOCTOU re-validation.
///
/// Also handles anonymous VMAs (stack, heap, BSS) that reach this path:
/// allocate a zeroed frame and map it directly.
async fn fault_in_page_async(task: &Arc<Task>, fault_va: VirtAddr) -> Result<(), Errno> {
    // 1. Look up VMA and compute object offsets
    let (obj, obj_offset, _vma_start, vma_perm, saved_ts) = {
        let mut map = task.vm_map.lock();
        let ts = map.timestamp.load(core::sync::atomic::Ordering::Relaxed);
        let vma = map.lookup(fault_va.as_usize() as u64).ok_or_else(|| {
            kerr!(
                vm,
                warn,
                Errno::Efault,
                "async fault NOT_MAPPED pid={} va={:#x}",
                task.pid,
                fault_va.as_usize()
            )
        })?;

        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));

        match &vma.store {
            crate::mm::vm::BackingStore::Object { object, offset } => {
                let obj_offset_bytes = offset + (fault_va_aligned.as_usize() as u64 - vma.start());
                let obj_offset =
                    crate::mm::vm::VObjIndex::from_bytes_floor(obj_offset_bytes as usize);
                (object.clone(), obj_offset, vma.start(), vma.protection, ts)
            },
            _ => {
                return Err(kerr!(
                    vm,
                    warn,
                    Errno::Efault,
                    "async fault INVALID_BACKING pid={} va={:#x}",
                    task.pid,
                    fault_va.as_usize()
                ))
            },
        }
    };

    // 2. Check if the page is already in the object (walks shadow chain)
    if let Some(existing_pa) = obj.read().lookup_page(obj_offset) {
        let map = task.vm_map.lock();
        // Revalidate if map mutated since our lookup
        let current_perm = if map.timestamp.load(core::sync::atomic::Ordering::Relaxed) != saved_ts
        {
            match map.lookup_readonly(fault_va.as_usize() as u64) {
                Some(vma) => vma.protection,
                None => return Ok(()), // VMA gone — silently succeed (no mapping needed)
            }
        } else {
            vma_perm
        };
        let mut pmap = map.pmap_lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        if pmap_extract(&pmap, fault_va_aligned).is_none() {
            if pmap_enter(
                &mut pmap,
                fault_va_aligned,
                existing_pa,
                current_perm,
                false,
            )
            .is_err()
            {
                return Err(kerr!(
                    vm,
                    error,
                    Errno::Enomem,
                    "async fault OOM: pmap_enter cached pid={} va={:#x}",
                    task.pid,
                    fault_va.as_usize()
                ));
            }
        }
        return Ok(());
    }

    // 2b. Shadow chain collapse: if our backing object has shadow_count == 1,
    // collapse the chain to shorten future lookups (BSD vm_object_collapse).
    // This prevents O(n) chain walks on fork-heavy workloads.
    {
        let mut obj_write = obj.write();
        if let Some(backing) = obj_write.backing_object() {
            if backing.read().shadow_count() == 1 {
                obj_write.collapse();
            }
        }
    }

    // 3. Fetch pager reference and allocate a frame.
    // We unconditionally zero it so that if the pager reads fewer than 4096 bytes (e.g. at EOF),
    // the unread portion remains zero, as required for BSS segments.
    let pager = { obj.read().pager.clone() };

    // Use FileCache role for file-backed pages so the page daemon can distinguish them.
    let role = match pager.as_ref() {
        Some(p) if !p.is_anon() => crate::mm::PageRole::FileCache,
        _ => crate::mm::PageRole::UserAnon,
    };
    let frame = crate::mm::alloc_raw_frame_sync(role).ok_or_else(|| {
        kerr!(
            vm,
            error,
            Errno::Enomem,
            "async fault OOM: frame alloc pid={} va={:#x}",
            task.pid,
            fault_va.as_usize()
        )
    })?;
    pmap_zero_page(frame);

    // 4. Fetch data via pager
    if let Some(pager_ref) = pager.as_ref() {
        let file_offset = obj_offset.to_bytes();
        if pager_ref.page_in(file_offset, frame).await.is_err() {
            // Decrement FRAME_META refs and free via PageRef drop.
            drop(PageRef::new(frame));
            return Err(kerr!(
                vm,
                error,
                Errno::Eio,
                "async fault IO_ERROR: page_in pid={} va={:#x} offset={:#x}",
                task.pid,
                fault_va.as_usize(),
                file_offset
            ));
        }
    }

    // 5. Time-of-use revalidation and mapping
    {
        let map = task.vm_map.lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));

        // Revalidate: did the map change during async I/O?
        let current_perm = if map.timestamp.load(core::sync::atomic::Ordering::Relaxed) != saved_ts
        {
            if let Some(vma) = map.lookup_readonly(fault_va.as_usize() as u64) {
                vma.protection
            } else {
                // VMA removed during async I/O (munmap raced). Discard the page.
                drop(PageRef::new(frame));
                return Ok(());
            }
        } else {
            vma_perm
        };

        let mut pmap = map.pmap_lock();
        let mut obj_write = obj.write();

        // Check if another thread raced us
        if let Some(existing_pa) = obj_write.lookup_page(obj_offset) {
            // Another thread already inserted a page. Free ours via PageRef drop.
            drop(PageRef::new(frame));
            if pmap_extract(&pmap, fault_va_aligned).is_none() {
                if pmap_enter(
                    &mut pmap,
                    fault_va_aligned,
                    existing_pa,
                    current_perm,
                    false,
                )
                .is_err()
                {
                    return Err(kerr!(
                        vm,
                        error,
                        Errno::Enomem,
                        "async fault OOM: pmap_enter race pid={} va={:#x}",
                        task.pid,
                        fault_va.as_usize()
                    ));
                }
            }
            return Ok(());
        }

        obj_write.insert_page(obj_offset, PageRef::new(frame));

        if pmap_extract(&pmap, fault_va_aligned).is_none() {
            if pmap_enter(&mut pmap, fault_va_aligned, frame, current_perm, false).is_err() {
                return Err(kerr!(
                    vm,
                    error,
                    Errno::Enomem,
                    "async fault OOM: pmap_enter final pid={} va={:#x}",
                    task.pid,
                    fault_va.as_usize()
                ));
            }
        }
    }

    Ok(())
}
