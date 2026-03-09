//! Memory management system calls.
//!
//! Implements mmap, munmap, brk, mprotect and related memory operations.

use crate::hal_common::{Errno, VirtAddr, PAGE_SIZE};
use alloc::sync::Arc;

use crate::proc::task::Task;

const MAP_PRIVATE: usize = 0x02;
const MAP_FIXED: usize = 0x10;
const MAP_ANONYMOUS: usize = 0x20;

fn is_page_aligned(addr: usize) -> bool {
    addr & (PAGE_SIZE - 1) == 0
}

fn align_up_to_page(value: usize) -> Option<usize> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|aligned| aligned & !(PAGE_SIZE - 1))
}

fn prot_bits_to_perm(prot_bits: usize) -> crate::mm::vm::map::entry::MapPerm {
    use crate::mm::vm::map::entry::MapPerm;

    let mut perm = MapPerm::U;
    if prot_bits & 0x1 != 0 {
        perm |= MapPerm::R;
    }
    if prot_bits & 0x2 != 0 {
        perm |= MapPerm::W;
    }
    if prot_bits & 0x4 != 0 {
        perm |= MapPerm::X;
    }
    perm
}

/// Free frames from removed VMAs.
/// Pages are freed automatically via RAII (TypedFrame Drop) when the
/// VmArea and its VmObject are dropped.
fn free_removed_frames(removed: alloc::vec::Vec<crate::mm::vm::map::entry::VmMapEntry>) {
    drop(removed);
}

/// sys_mmap: real mmap with top-down allocation and MAP_FIXED.
pub fn sys_mmap(
    task: &Arc<Task>,
    addr: usize,
    len: usize,
    prot_bits: usize,
    flags: usize,
    fd: u32,
    offset: u64,
) -> usize {
    use crate::mm::vm::map::entry::{BackingStore, EntryFlags, MapPerm, VmMapEntry};
    use crate::mm::vm::vm_object::VmObject;

    let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    if aligned_len == 0 {
        return (-(Errno::Einval.as_i32() as isize)) as usize;
    }

    let map_fixed = flags & MAP_FIXED != 0;
    let map_anon = flags & MAP_ANONYMOUS != 0;
    let map_private = flags & MAP_PRIVATE != 0;

    // Current VM syscall layer only supports anonymous private mappings.
    // File-backed mmap should be wired through vnode-backed VmObject + pager,
    // not silently downgraded to anonymous memory.
    if !map_anon || !map_private {
        return (-(Errno::Enosys.as_i32() as isize)) as usize;
    }

    // Linux ignores fd for MAP_ANONYMOUS, but offset must still be page-aligned.
    // Keep the interface strict here to match the current implementation.
    if offset != 0 {
        return (-(Errno::Einval.as_i32() as isize)) as usize;
    }
    let _ = fd;

    let mut vm = task.vm_map.lock();

    let base = if map_fixed {
        let start = VirtAddr::new(addr & !0xFFF);
        let end = VirtAddr::new(start.as_usize() + aligned_len);
        // MAP_FIXED: delete existing mappings in range first
        let removed = vm.remove_range(start, end);
        // Free anonymous frames from removed VMAs
        free_removed_frames(removed);
        start.as_usize()
    } else if addr != 0 {
        // Hint address: try it, fall back to top-down
        let hint = VirtAddr::new(addr & !0xFFF);
        let hint_end = VirtAddr::new(hint.as_usize() + aligned_len);
        // Check if hint range is free
        let hint_ok = vm.is_range_free(hint.as_usize() as u64, hint_end.as_usize() as u64);
        if hint_ok {
            hint.as_usize()
        } else {
            match vm.find_free_area_topdown(aligned_len) {
                Some(va) => va.as_usize(),
                None => return (-(Errno::Enomem.as_i32() as isize)) as usize,
            }
        }
    } else {
        // Top-down allocation
        match vm.find_free_area_topdown(aligned_len) {
            Some(va) => va.as_usize(),
            None => return (-(Errno::Enomem.as_i32() as isize)) as usize,
        }
    };

    // Build VMA
    let perm: MapPerm = prot_bits_to_perm(prot_bits);

    let obj = VmObject::new_anon(aligned_len);
    let vma = VmMapEntry::new(
        base as u64,
        (base + aligned_len) as u64,
        BackingStore::Object {
            object: obj,
            offset: 0,
        },
        EntryFlags::empty(),
        perm,
    );
    match vm.insert_entry(vma) {
        Ok(()) => base,
        Err(_) => (-(Errno::Enomem.as_i32() as isize)) as usize,
    }
}

/// sys_munmap: tear down PTEs + TLB + remove/split VMAs.
pub fn sys_munmap(task: &Arc<Task>, addr: usize, len: usize) -> usize {
    if len == 0 || !is_page_aligned(addr) {
        return (-(Errno::Einval.as_i32() as isize)) as usize;
    }

    let end = match addr.checked_add(len) {
        Some(end) => end,
        None => return (-(Errno::Einval.as_i32() as isize)) as usize,
    };

    let aligned_end = match align_up_to_page(end) {
        Some(end) => VirtAddr::new(end),
        None => return (-(Errno::Einval.as_i32() as isize)) as usize,
    };
    let aligned_start = VirtAddr::new(addr);
    if aligned_start >= aligned_end {
        return (-(Errno::Einval.as_i32() as isize)) as usize;
    }
    let mut vm = task.vm_map.lock();
    let removed = vm.remove_range(aligned_start, aligned_end);
    free_removed_frames(removed);
    0
}

/// sys_mprotect: change VMA permissions + update PTEs.
pub fn sys_mprotect(task: &Arc<Task>, addr: usize, len: usize, prot_bits: usize) -> usize {
    let start = VirtAddr::new(addr & !0xFFF);
    let end = VirtAddr::new((addr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    if start >= end {
        return (-(Errno::Einval.as_i32() as isize)) as usize;
    }

    let perm = prot_bits_to_perm(prot_bits);

    let mut vm = task.vm_map.lock();
    match vm.protect_range(start, end, perm) {
        Ok(()) => 0,
        Err(_) => (-(Errno::Einval.as_i32() as isize)) as usize,
    }
}
/// sys_brk: change program break (heap end).
pub fn sys_brk(task: &Arc<Task>, addr: usize) -> usize {
    use core::sync::atomic::Ordering;

    let current_brk = task.brk.load(Ordering::Relaxed);
    if addr == 0 {
        return current_brk;
    }

    let requested_brk = addr;
    let new_brk_aligned = (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let old_brk_aligned = (current_brk + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    if new_brk_aligned == old_brk_aligned {
        task.brk.store(requested_brk, Ordering::Relaxed);
        return requested_brk;
    }

    let mut vm = task.vm_map.lock();

    if new_brk_aligned > old_brk_aligned {
        if vm.grow_heap(old_brk_aligned, new_brk_aligned).is_err() {
            return current_brk;
        }
    } else if new_brk_aligned < old_brk_aligned {
        match vm.shrink_heap(old_brk_aligned, new_brk_aligned) {
            Ok(removed) => free_removed_frames(removed),
            Err(_) => return current_brk,
        }
    }

    task.brk.store(requested_brk, Ordering::Relaxed);
    requested_brk
}
