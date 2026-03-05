//! Asynchronous page fault resolution.
//!
//! Handles demand paging for both anonymous and file-backed memory regions,
//! with page cache integration for file-backed pages.

use alloc::string::String;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use hal_common::{PhysAddr, VirtAddr, PAGE_SIZE};
use hal_common::addr::VirtPageNum;

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
        let pmap = task.pmap.lock();
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
        }
    }

    // 1. Sync path: anonymous zero-fill, COW
    let sync_result = {
        let vm_map = task.vm_map.lock();
        let mut pmap = task.pmap.lock();
        sync_fault_handler(&vm_map, &mut pmap, fault_va, access_type)
    };

    match sync_result {
        FaultResult::Resolved => Ok(()),
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
                Ok(()) => crate::klog!(
                    vm,
                    trace,
                    "resolve_user_fault: async OK pid={} va={:#x}",
                    task.pid,
                    fault_va.as_usize()
                ),
                Err(e) => crate::klog!(
                    vm,
                    trace,
                    "resolve_user_fault: async FAILED pid={} va={:#x} err={:?}",
                    task.pid,
                    fault_va.as_usize(),
                    e
                ),
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
    // Time-of-check: snapshot VMA state under lock
    let (vnode_id, vnode_path, file_offset, file_size, _vma_file_offset, vma_start) = {
        let map = task.vm_map.lock();
        let vma = map.find_area(fault_va).ok_or(FaultError::NotMapped)?;
        let vnode = match vma.vnode.as_ref() {
            Some(v) => v,
            None => {
                // Anonymous VMA (stack, heap, BSS): allocate zeroed frame and map.
                let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
                let obj_offset = VirtPageNum(
                    (fault_va_aligned.as_usize() - vma.range.start.as_usize()) / PAGE_SIZE
                        + vma.obj_offset.as_usize(),
                );
                let prot = vma.prot;

                // TOCTOU: another core may have already resolved this fault.
                {
                    let obj = vma.object.read();
                    if let Some(existing) = obj.lookup_page(obj_offset) {
                        let mut pmap = task.pmap.lock();
                        if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
                            let _ = crate::mm::pmap::pmap_enter(
                                &mut pmap,
                                fault_va_aligned,
                                existing,
                                prot,
                                false,
                            );
                        }
                        return Ok(());
                    }
                }

                let frame =
                    crate::mm::allocator::frame_alloc_sync().ok_or(FaultError::OutOfMemory)?;
                crate::mm::pmap::pmap_zero_page(frame);

                {
                    let mut obj = vma.object.write();
                    // Re-check under write lock to avoid double-insert race.
                    if let Some(existing) = obj.lookup_page(obj_offset) {
                        // Another core won — use their page, free ours.
                        drop(obj);
                        crate::mm::allocator::frame_free(frame);
                        let mut pmap = task.pmap.lock();
                        if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
                            let _ = crate::mm::pmap::pmap_enter(
                                &mut pmap,
                                fault_va_aligned,
                                existing,
                                prot,
                                false,
                            );
                        }
                        return Ok(());
                    }
                    let typed_frame = crate::mm::allocator::TypedFrame {
                        phys: frame,
                        _marker: core::marker::PhantomData::<crate::mm::allocator::UserAnon>,
                    };
                    obj.insert_page(
                        obj_offset,
                        crate::mm::vm::vm_object::OwnedPage::new_anonymous(typed_frame),
                    );
                }

                let mut pmap = task.pmap.lock();
                if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
                    let _ = crate::mm::pmap::pmap_enter(
                        &mut pmap,
                        fault_va_aligned,
                        frame,
                        prot,
                        false,
                    );
                }
                return Ok(());
            }
        };
        let page_idx = (fault_va.as_usize() - vma.range.start.as_usize()) / PAGE_SIZE;
        let file_offset = vma.file_offset + page_idx * PAGE_SIZE;
        // Debug: log file-backed fault details
        crate::klog!(vm, trace, "fault_in_page_async pid={} va={:#x} vnode={} path={} file_offset={:#x} file_size={:#x} vma_start={:#x} page_idx={}",
            task.pid, fault_va.as_usize(), vnode.vnode_id(), vnode.path(), file_offset, vma.file_size, vma.range.start.as_usize(), page_idx);
        (
            vnode.vnode_id(),
            String::from(vnode.path()),
            file_offset,
            vma.file_size,
            vma.file_offset,
            vma.range.start.as_usize(),
        )
    }; // lock dropped before .await

    // Compute how far into the VMA this page starts (in bytes)
    let fault_va_aligned = fault_va.as_usize() & !(PAGE_SIZE - 1);
    let vma_page_byte_offset = fault_va_aligned - vma_start;

    let pa = if vma_page_byte_offset >= file_size {
        // Entirely beyond file data — pure BSS zero page
        let frame = crate::mm::allocator::frame_alloc_sync().ok_or(FaultError::OutOfMemory)?;
        unsafe {
            core::ptr::write_bytes(frame.as_usize() as *mut u8, 0, PAGE_SIZE);
        }
        frame
    } else if vma_page_byte_offset + PAGE_SIZE > file_size {
        // Partial page: file data + zero fill for the rest
        let fetched = page_cache_fetch_by_id(vnode_id, &vnode_path, file_offset as u64).await?;
        // Copy file portion to a new frame and zero the tail
        let frame = crate::mm::allocator::frame_alloc_sync().ok_or(FaultError::OutOfMemory)?;
        let file_bytes = file_size - vma_page_byte_offset;
        unsafe {
            let src_slice = fetched.as_slice();
            let dst_slice = frame.as_mut_slice();
            dst_slice[..file_bytes].copy_from_slice(&src_slice[..file_bytes]);
            dst_slice[file_bytes..].fill(0);
        }
        frame
    } else {
        // Fully within file data — use page cache directly
        page_cache_fetch_by_id(vnode_id, &vnode_path, file_offset as u64).await?
    };

    // Time-of-use: re-validate VMA under lock before mapping
    {
        let map = task.vm_map.lock();
        let vma = map.find_area(fault_va).ok_or(FaultError::NotMapped)?;
        let expected_offset = vma.file_offset
            + ((fault_va.as_usize() - vma.range.start.as_usize()) & !(PAGE_SIZE - 1));
        let current_id = vma.vnode.as_ref().map(|v| v.vnode_id());
        if current_id != Some(vnode_id) || expected_offset != file_offset {
            return Err(FaultError::InvalidAccess);
        }

        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        let mut pmap = task.pmap.lock();

        // Guard: if the page is already mapped, don't overwrite it.
        // This can happen when fault_in_user_buffer pre-faults a page that
        // was already demand-paged in by a real hardware fault.
        if let Some(_existing_pa) = crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned) {
            // Free the frame we just allocated (BSS/partial paths) to avoid leak.
            // For the FILE path, `pa` came from the page cache — don't free it.
            if vma_page_byte_offset >= file_size
                || vma_page_byte_offset + PAGE_SIZE > file_size
            {
                crate::mm::allocator::frame_free(pa);
            }
            return Ok(());
        }

        let _ = crate::mm::pmap::pmap_enter(&mut pmap, fault_va_aligned, pa, vma.prot, false);
    }

    Ok(())
}

/// Fetch a page from the page cache, initiating I/O if needed.
pub async fn page_cache_fetch_by_id(
    vnode_id: u64,
    path: &str,
    file_offset: u64,
) -> Result<PhysAddr, FaultError> {
    use crate::fs::page_cache::{self, LookupResult};

    let page_offset = file_offset / PAGE_SIZE as u64;

    loop {
        // Use a no-op waker for the non-blocking probe path.
        let noop = noop_waker();
        let result = page_cache::lookup(vnode_id, page_offset, &noop);
        match result {
            LookupResult::Hit(pa) => return Ok(pa),
            LookupResult::InitiateFetch => {
                // We're responsible for fetching this page.
                match crate::fs::delegate::fs_read_page(path, file_offset).await {
                    Ok(pa_usize) => {
                        let pa = PhysAddr::new(pa_usize);
                        page_cache::complete(vnode_id, page_offset, pa);
                        return Ok(pa);
                    }
                    Err(_) => {
                        // TODO: page_cache::abort(vnode_id, page_offset);
                        return Err(FaultError::IoError);
                    }
                }
            }
            LookupResult::WaitingOnFetch => {
                // Another task is fetching. Wait via proper future with real waker.
                PageCacheWaitFuture {
                    vnode_id,
                    page_offset,
                }
                .await;
            }
        }
    }
}

/// Future that waits for a page cache fetch to complete.
struct PageCacheWaitFuture {
    vnode_id: u64,
    page_offset: u64,
}

impl Future for PageCacheWaitFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        use crate::fs::page_cache::{self, LookupResult};
        match page_cache::lookup(self.vnode_id, self.page_offset, cx.waker()) {
            LookupResult::Hit(_) => Poll::Ready(()),
            LookupResult::WaitingOnFetch => Poll::Pending,
            LookupResult::InitiateFetch => Poll::Ready(()), // race: caller loop handles it
        }
    }
}

/// Create a no-op waker (for page_cache::lookup when we don't need to wait).
fn noop_waker() -> core::task::Waker {
    use core::task::{RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone_fn(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
}
