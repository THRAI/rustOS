//! Memory management system calls.
//!
//! Implements mmap, munmap, brk, mprotect and related memory operations.

use alloc::sync::Arc;
use hal_common::{Errno, VirtAddr, PAGE_SIZE};

use crate::proc::task::Task;

/// Free frames from removed VMAs.
/// Pages are freed automatically via RAII (TypedFrame Drop) when the
/// VmArea and its VmObject are dropped.
fn free_removed_frames(removed: alloc::vec::Vec<crate::mm::vm::vm_map::VmArea>) {
    drop(removed);
}

/// sys_mmap: real mmap with top-down allocation and MAP_FIXED.
pub fn sys_mmap(
    task: &Arc<Task>,
    addr: usize,
    len: usize,
    prot_bits: usize,
    flags: usize,
    _fd: u32,
    _offset: u64,
) -> usize {
    use crate::mm::vm::vm_map::{MapPerm, VmArea, VmAreaType};
    use crate::mm::vm::vm_object::VmObject;

    let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    if aligned_len == 0 {
        return (-(Errno::EINVAL.as_i32() as isize)) as usize;
    }

    let map_fixed = flags & 0x10 != 0; // MAP_FIXED
    let _map_anon = flags & 0x20 != 0; // MAP_ANONYMOUS
    let _map_private = flags & 0x02 != 0; // MAP_PRIVATE

    let mut vm = task.vm_map.lock();

    let base = if map_fixed {
        let start = VirtAddr::new(addr & !0xFFF);
        let end = VirtAddr::new(start.as_usize() + aligned_len);
        // MAP_FIXED: delete existing mappings in range first
        let removed = vm.remove_range(start, end);
        // Tear down PTEs for removed range
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_remove(&mut pmap, start, end);
        }
        // Free anonymous frames from removed VMAs
        free_removed_frames(removed);
        start.as_usize()
    } else if addr != 0 {
        // Hint address: try it, fall back to top-down
        let hint = VirtAddr::new(addr & !0xFFF);
        let hint_end = VirtAddr::new(hint.as_usize() + aligned_len);
        // Check if hint range is free
        let hint_ok = vm.find_area(hint).is_none()
            && vm
                .find_area(VirtAddr::new(hint_end.as_usize().saturating_sub(1)))
                .is_none();
        if hint_ok {
            hint.as_usize()
        } else {
            match vm.find_free_area_topdown(aligned_len) {
                Some(va) => va.as_usize(),
                None => return (-(Errno::ENOMEM.as_i32() as isize)) as usize,
            }
        }
    } else {
        // Top-down allocation
        match vm.find_free_area_topdown(aligned_len) {
            Some(va) => va.as_usize(),
            None => return (-(Errno::ENOMEM.as_i32() as isize)) as usize,
        }
    };

    // Build VMA
    let mut perm = MapPerm::U;
    if prot_bits & 1 != 0 {
        perm |= MapPerm::R;
    }
    if prot_bits & 2 != 0 {
        perm |= MapPerm::W;
    }
    if prot_bits & 4 != 0 {
        perm |= MapPerm::X;
    }

    let obj = VmObject::new(aligned_len / PAGE_SIZE);
    let vma = VmArea::new(
        VirtAddr::new(base)..VirtAddr::new(base + aligned_len),
        perm,
        obj,
        hal_common::addr::VirtPageNum(0),
        VmAreaType::Anonymous,
    );
    match vm.insert(vma) {
        Ok(()) => base,
        Err(_) => (-(Errno::ENOMEM.as_i32() as isize)) as usize,
    }
}

/// sys_munmap: tear down PTEs + TLB + remove/split VMAs.
pub fn sys_munmap(task: &Arc<Task>, addr: usize, len: usize) -> usize {
    let aligned_start = VirtAddr::new(addr & !0xFFF);
    let aligned_end = VirtAddr::new((addr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    if aligned_start >= aligned_end {
        return (-(Errno::EINVAL.as_i32() as isize)) as usize;
    }
    let mut vm = task.vm_map.lock();
    let removed = vm.remove_range(aligned_start, aligned_end);
    {
        let mut pmap = task.pmap.lock();
        crate::mm::pmap::pmap_remove(&mut pmap, aligned_start, aligned_end);
    }
    free_removed_frames(removed);
    0
}

/// sys_mprotect: change VMA permissions + update PTEs.
pub fn sys_mprotect(task: &Arc<Task>, addr: usize, len: usize, prot_bits: usize) -> usize {
    use crate::mm::vm::vm_map::MapPerm;

    let start = VirtAddr::new(addr & !0xFFF);
    let end = VirtAddr::new((addr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    if start >= end {
        return (-(Errno::EINVAL.as_i32() as isize)) as usize;
    }

    let mut perm = MapPerm::U;
    if prot_bits & 1 != 0 {
        perm |= MapPerm::R;
    }
    if prot_bits & 2 != 0 {
        perm |= MapPerm::W;
    }
    if prot_bits & 4 != 0 {
        perm |= MapPerm::X;
    }

    let mut vm = task.vm_map.lock();
    vm.protect_range(start, end, perm);
    {
        let mut pmap = task.pmap.lock();
        crate::mm::pmap::pmap_protect(&mut pmap, start, end, perm);
    }
    0
}

/// sys_brk: change program break (heap end).
pub fn sys_brk(task: &Arc<Task>, addr: usize) -> usize {
    use crate::mm::vm::vm_map::{VmArea, VmAreaType};
    use crate::mm::vm::vm_object::VmObject;
    use hal_common::addr::VirtPageNum;

    let current_brk = task.brk.load(core::sync::atomic::Ordering::Relaxed);
    if addr == 0 {
        // Query current brk
        return current_brk;
    }

    let new_brk = (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let old_brk = current_brk;
    
    if new_brk > old_brk {
        // --- Expand ---
        let mut vm = task.vm_map.lock();
        if let Some(heap_vma) =
            vm.find_area_ending_at_mut(VirtAddr::new(old_brk), VmAreaType::Heap)
        {
            // In-place extension: slide end, grow VmObject
            heap_vma.range.end = VirtAddr::new(new_brk);
            let mut obj = heap_vma.object.write();
            obj.set_size(new_brk - heap_vma.range.start.as_usize());
        } else {
            // First brk or no adjacent heap VMA — create new
            let size = new_brk - old_brk;
            let obj = VmObject::new(size);
            let vma = VmArea::new(
                VirtAddr::new(old_brk)..VirtAddr::new(new_brk),
                crate::map_perm!(R, W, U),
                obj,
                hal_common::addr::VirtPageNum(0),
                VmAreaType::Heap,
            );
            if vm.insert(vma).is_err() {
                return current_brk;
            }
        }
    } else if new_brk < old_brk {
        // --- Shrink ---
        // 1. Tear down hardware PTEs + TLB shootdown
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_remove(
                &mut pmap,
                VirtAddr::new(new_brk),
                VirtAddr::new(old_brk),
            );
        }
        // 2. Truncate VmObject pages and free anonymous frames
        let mut vm = task.vm_map.lock();
        // Find the heap VMA that contains old_brk - 1
        if let Some(heap_vma) = vm.find_area_mut(VirtAddr::new(old_brk - 1)) {
            if heap_vma.vma_type == VmAreaType::Heap {
                let vma_start = heap_vma.range.start.as_usize();
                let from_page = VirtPageNum((new_brk - vma_start) / PAGE_SIZE);
                // Truncate pages from VmObject (top-level only — COW safe).
                // Pages are freed automatically via RAII (TypedFrame Drop).
                {
                    let mut obj = heap_vma.object.write();
                    let _freed = obj.truncate_pages(from_page);
                    obj.set_size(new_brk.saturating_sub(vma_start));
                    // _freed drops here, freeing frames via TypedFrame RAII
                }
                // Slide VMA end down (or remove if fully shrunk)
                if new_brk <= vma_start {
                    vm.remove(VirtAddr::new(vma_start));
                } else {
                    // Re-lookup since we dropped the borrow for frame_free
                    if let Some(vma) = vm.find_area_mut(VirtAddr::new(vma_start)) {
                        vma.range.end = VirtAddr::new(new_brk);
                    }
                }
            }
        }
    }
    
    task.brk.store(new_brk, core::sync::atomic::Ordering::Relaxed);
    new_brk
}
