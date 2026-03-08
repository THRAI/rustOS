//! Memory management system calls.
//!
//! Implements mmap, munmap, brk, mprotect and related memory operations.

use crate::hal_common::{Errno, VirtAddr, PAGE_SIZE};
use alloc::sync::Arc;

use crate::proc::task::Task;

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

    let map_fixed = flags & 0x10 != 0; // MAP_FIXED
    let map_anon = flags & 0x20 != 0; // MAP_ANONYMOUS
    let _map_private = flags & 0x02 != 0; // MAP_PRIVATE

    let mut vm = task.vm_map.lock();

    let base = if map_fixed {
        let start = VirtAddr::new(addr & !0xFFF);
        let end = VirtAddr::new(start.as_usize() + aligned_len);
        // MAP_FIXED: delete existing mappings in range first
        let removed = vm.remove_range(start, end);
        // Tear down PTEs for removed range
        {
            let mut pmap = vm.pmap_lock();
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

    let obj = VmObject::new_anon(aligned_len / PAGE_SIZE);
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
    let aligned_start = VirtAddr::new(addr & !0xFFF);
    let aligned_end = VirtAddr::new((addr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    if aligned_start >= aligned_end {
        return (-(Errno::Einval.as_i32() as isize)) as usize;
    }
    let mut vm = task.vm_map.lock();
    let removed = vm.remove_range(aligned_start, aligned_end);
    {
        let mut pmap = vm.pmap_lock();
        crate::mm::pmap::pmap_remove(&mut pmap, aligned_start, aligned_end);
    }
    free_removed_frames(removed);
    0
}

/// sys_mprotect: change VMA permissions + update PTEs.
pub fn sys_mprotect(task: &Arc<Task>, addr: usize, len: usize, prot_bits: usize) -> usize {
    use crate::mm::vm::map::entry::MapPerm;

    let start = VirtAddr::new(addr & !0xFFF);
    let end = VirtAddr::new((addr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    if start >= end {
        return (-(Errno::Einval.as_i32() as isize)) as usize;
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
        let mut pmap = vm.pmap_lock();
        crate::mm::pmap::pmap_protect(&mut pmap, start, end, perm);
    }
    0
}

/// sys_brk: change program break (heap end).
pub fn sys_brk(task: &Arc<Task>, addr: usize) -> usize {
    use crate::hal_common::addr::VirtPageNum;
    use crate::mm::vm::map::entry::{BackingStore, EntryFlags, MapPerm, VmMapEntry};
    use crate::mm::vm::vm_object::VmObject;
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
        // Prefer in-place extension of the existing heap VMA.
        let mut extended = false;
        if old_brk_aligned > 0 {
            if let Some(vma) = vm.lookup_mut((old_brk_aligned - 1) as u64) {
                if vma.end() == old_brk_aligned as u64 && vma.flags.contains(EntryFlags::HEAP) {
                    let heap_start = vma.start();
                    vma.set_bounds(heap_start, new_brk_aligned as u64);
                    if let BackingStore::Object { object, .. } = &vma.store {
                        let mut obj = object.write();
                        obj.set_size((new_brk_aligned - heap_start as usize) / PAGE_SIZE);
                    }
                    extended = true;
                }
            }
        }

        // First brk growth creates a dedicated heap VMA.
        if !extended {
            if !vm.is_range_free(old_brk_aligned as u64, new_brk_aligned as u64) {
                return current_brk;
            }
            let grow_len = new_brk_aligned - old_brk_aligned;
            let obj = VmObject::new_anon(grow_len / PAGE_SIZE);
            let vma = VmMapEntry::new(
                old_brk_aligned as u64,
                new_brk_aligned as u64,
                BackingStore::Object {
                    object: obj,
                    offset: 0,
                },
                EntryFlags::HEAP,
                MapPerm::R | MapPerm::W | MapPerm::U,
            );
            if vm.insert_entry(vma).is_err() {
                return current_brk;
            }
        }
    } else if new_brk_aligned < old_brk_aligned {
        // Shrink only the heap VMA; do not tear down unrelated VMAs.
        let mut unmap_start: Option<usize> = None;
        let mut remove_heap_va: Option<u64> = None;

        if old_brk_aligned > 0 {
            if let Some(vma) = vm.lookup_mut((old_brk_aligned - 1) as u64) {
                if vma.end() == old_brk_aligned as u64 && vma.flags.contains(EntryFlags::HEAP) {
                    let heap_start = vma.start() as usize;
                    let new_heap_end = core::cmp::max(new_brk_aligned, heap_start);
                    unmap_start = Some(new_heap_end);

                    if new_heap_end <= heap_start {
                        remove_heap_va = Some((old_brk_aligned - 1) as u64);
                    } else {
                        vma.set_bounds(heap_start as u64, new_heap_end as u64);
                        if let BackingStore::Object { object, .. } = &vma.store {
                            let new_pages = (new_heap_end - heap_start) / PAGE_SIZE;
                            let mut obj = object.write();
                            obj.set_size(new_pages);
                            let dropped = obj.truncate_pages(VirtPageNum(new_pages));
                            drop(dropped);
                        }
                    }
                }
            }
        }

        if let Some(start) = unmap_start {
            if start < old_brk_aligned {
                let mut pmap = vm.pmap_lock();
                crate::mm::pmap::pmap_remove(
                    &mut pmap,
                    VirtAddr::new(start),
                    VirtAddr::new(old_brk_aligned),
                );
            }
        }
        if let Some(va) = remove_heap_va {
            if let Some(removed) = vm.remove_entry_containing(va) {
                free_removed_frames(alloc::vec![removed]);
            }
        }
    }

    task.brk.store(requested_brk, Ordering::Relaxed);
    requested_brk
}
