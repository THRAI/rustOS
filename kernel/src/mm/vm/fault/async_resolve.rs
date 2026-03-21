//! Asynchronous page fault resolution.
//!
//! Handles demand paging for both anonymous and file-backed memory regions,
//! with page cache integration for file-backed pages.

use alloc::sync::Arc;

use crate::{
    hal_common::{Errno, LeveledRwLock, VirtAddr, PAGE_SIZE},
    mm::{
        pmap_zero_page,
        vm::{
            sync_fault_handler, FaultResult, MapPerm, PageFaultAccessType, PageRef, VmMap, VmObject,
        },
    },
    proc::Task,
};

/// Re-check VMA after timestamp mismatch. Returns fresh permissions if
/// the VMA still exists and backs the same object (`Arc::ptr_eq`), or
/// `None` to bail (VMA gone or object identity changed).
///
/// This is the unified TOCTOU revalidation — it replaces the old
/// `IN_TRANSITION` and `busy_count` mechanisms with a single detection
/// protocol. See `VmMap.timestamp` doc comment for the full design.
///
/// If thread profiling later shows wasted I/O from concurrent
/// munmap-during-fault, a `busy_count` prevention layer can be added
/// on top without changing this function.
fn revalidate_vma(
    map: &VmMap,
    fault_va: VirtAddr,
    obj: &Arc<LeveledRwLock<VmObject, 3>>,
    saved_ts: u32,
    vma_perm: MapPerm,
) -> Option<MapPerm> {
    if map.timestamp.load(core::sync::atomic::Ordering::Relaxed) == saved_ts {
        return Some(vma_perm);
    }
    let vma = map.lookup_readonly(fault_va)?;
    // Verify backing object identity — after execve, the same VA may
    // exist with a different object. Inserting into the old object would
    // create a dangling PTE when the old object drops.
    if let Some(object) = vma.mapping.object() {
        if !Arc::ptr_eq(object, obj) {
            return None;
        }
    }
    Some(vma.protection)
}

/// Unified fault resolution: sync path first, async fallback for file-backed.
pub async fn resolve_user_fault(
    task: &Arc<Task>,
    fault_va: VirtAddr,
    access_type: PageFaultAccessType,
) -> Result<(), Errno> {
    // Fast path: if the page is already mapped with sufficient permissions, skip.
    // Must check PTE flags — not just presence — to avoid bypassing COW faults.
    {
        let vm_map = task.vm_map.read();
        let pmap = vm_map.pmap_lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        if let Some(entry) = pmap.get(fault_va_aligned) {
            let mut ok = true;
            if access_type.write && !entry.permits_write() {
                ok = false; // COW page — must go through fault handler
            }
            if access_type.execute && !entry.permits_exec() {
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
                entry.flags(),
                access_type.write,
                access_type.execute
            );
        }
    }

    // 1. Sync path: anonymous zero-fill, COW
    let sync_result = {
        let vm_map = task.vm_map.read();
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
        let map = task.vm_map.read();
        let ts = map.timestamp.load(core::sync::atomic::Ordering::Relaxed);
        let vma = map.lookup_readonly(fault_va).ok_or_else(|| {
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

        let (object, obj_offset) =
            vma.extract_object_offset(fault_va_aligned).ok_or_else(|| {
                kerr!(
                    vm,
                    warn,
                    Errno::Efault,
                    "async fault INVALID_BACKING pid={} va={:#x}",
                    task.pid,
                    fault_va.as_usize()
                )
            })?;
        (object, obj_offset, vma.start(), vma.protection, ts)
    };

    // 2. Check if the page is already in the object (walks shadow chain)
    if let Some(existing_pa) = obj.read().lookup_page(obj_offset) {
        let map = task.vm_map.read();
        let current_perm = match revalidate_vma(&map, fault_va, &obj, saved_ts, vma_perm) {
            Some(perm) => perm,
            None => return Ok(()), // VMA gone or object identity changed
        };
        let mut pmap = map.pmap_lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        if pmap
            .entry_or_insert(fault_va_aligned, existing_pa, current_perm)
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
        let map = task.vm_map.read();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));

        let current_perm =
            if let Some(perm) = revalidate_vma(&map, fault_va, &obj, saved_ts, vma_perm) {
                perm
            } else {
                // VMA gone or object identity changed — discard frame.
                drop(PageRef::new(frame));
                return Ok(());
            };

        let mut pmap = map.pmap_lock();
        let mut obj_write = obj.write();

        // Check if another thread raced us
        if let Some(existing_pa) = obj_write.lookup_page(obj_offset) {
            // Another thread already inserted a page. Free ours via PageRef drop.
            drop(PageRef::new(frame));
            if pmap
                .entry_or_insert(fault_va_aligned, existing_pa, current_perm)
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
            return Ok(());
        }

        obj_write.insert_page(obj_offset, PageRef::new(frame));

        if pmap
            .entry_or_insert(fault_va_aligned, frame, current_perm)
            .is_err()
        {
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

    Ok(())
}
