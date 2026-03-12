//! Memory management system calls.
//!
//! Implements mmap, munmap, brk, mprotect and related memory operations.

use alloc::sync::Arc;

use crate::{
    fs::FileObject,
    hal_common::{Errno, VirtAddr, PAGE_SIZE},
    proc::Task,
};

const PROT_READ: usize = 0x1;
const PROT_WRITE: usize = 0x2;
const PROT_EXEC: usize = 0x4;

const MAP_SHARED: usize = 0x01;
const MAP_PRIVATE: usize = 0x02;
const MAP_TYPE_MASK: usize = 0x0f;
const MAP_FIXED: usize = 0x10;
const MAP_ANONYMOUS: usize = 0x20;
const SUPPORTED_MMAP_FLAGS: usize = MAP_TYPE_MASK | MAP_FIXED | MAP_ANONYMOUS;

fn errno_ret(errno: Errno) -> usize {
    (-(errno.as_i32() as isize)) as usize
}

fn is_page_aligned(addr: usize) -> bool {
    addr & (PAGE_SIZE - 1) == 0
}

fn align_up_to_page(value: usize) -> Option<usize> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|aligned| aligned & !(PAGE_SIZE - 1))
}

fn prot_bits_to_perm(prot_bits: usize) -> Result<crate::mm::vm::MapPerm, Errno> {
    use crate::mm::vm::MapPerm;

    if prot_bits & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(Errno::Einval);
    }

    let mut perm = MapPerm::U;
    if prot_bits & PROT_READ != 0 {
        perm |= MapPerm::R;
    }
    if prot_bits & PROT_WRITE != 0 {
        perm |= MapPerm::W;
    }
    if prot_bits & PROT_EXEC != 0 {
        perm |= MapPerm::X;
    }
    Ok(perm)
}

fn free_removed_frames(removed: alloc::vec::Vec<crate::mm::vm::VmMapEntry>) {
    drop(removed);
}

fn resolve_mmap_base(
    vm: &mut crate::mm::vm::VmMap,
    addr: usize,
    aligned_len: usize,
    map_fixed: bool,
) -> Result<usize, Errno> {
    if map_fixed {
        if !is_page_aligned(addr) {
            return Err(Errno::Einval);
        }
        let end = addr.checked_add(aligned_len).ok_or(Errno::Einval)?;
        let removed = vm.remove_range(VirtAddr::new(addr), VirtAddr::new(end));
        free_removed_frames(removed);
        return Ok(addr);
    }

    if addr != 0 {
        let hint = addr & !(PAGE_SIZE - 1);
        let hint_end = hint.checked_add(aligned_len).ok_or(Errno::Einval)?;
        if vm.is_range_free(hint as u64, hint_end as u64) {
            return Ok(hint);
        }
    }

    vm.find_free_area_topdown(aligned_len)
        .map(|va| va.as_usize())
        .ok_or(Errno::Enomem)
}

fn build_file_backed_object(
    vnode: &Arc<dyn crate::fs::Vnode>,
    map_private: bool,
) -> Arc<spin::RwLock<crate::mm::vm::VmObject>> {
    let base_obj = crate::fs::vnode_object(&**vnode);

    if !map_private {
        return base_obj;
    }

    let shadow = crate::mm::vm::VmObject::new_shadow(Arc::clone(&base_obj), base_obj.read().size());
    if let Some(ref pager) = base_obj.read().pager {
        if !pager.is_anon() {
            shadow.write().pager = Some(Arc::clone(pager));
        }
    }
    shadow
}

/// sys_mmap: build a VMA over the current VmMap/Object model.
pub fn sys_mmap(
    task: &Arc<Task>,
    addr: usize,
    len: usize,
    prot_bits: usize,
    flags: usize,
    fd: u32,
    offset: u64,
) -> usize {
    use crate::{
        fs::VnodeType,
        mm::vm::{BackingStore, EntryFlags, MapPerm, VmMapEntry, VmObject},
    };

    let aligned_len = match align_up_to_page(len) {
        Some(aligned_len) if aligned_len != 0 => aligned_len,
        _ => return errno_ret(Errno::Einval),
    };

    if flags & !SUPPORTED_MMAP_FLAGS != 0 {
        return errno_ret(Errno::Einval);
    }

    let map_type = flags & MAP_TYPE_MASK;
    let map_shared = map_type == MAP_SHARED;
    let map_private = map_type == MAP_PRIVATE;
    if !map_shared && !map_private {
        return errno_ret(Errno::Einval);
    }

    let map_fixed = flags & MAP_FIXED != 0;
    let map_anon = flags & MAP_ANONYMOUS != 0;

    let perm: MapPerm = match prot_bits_to_perm(prot_bits) {
        Ok(perm) => perm,
        Err(errno) => return errno_ret(errno),
    };

    if offset % PAGE_SIZE as u64 != 0 {
        return errno_ret(Errno::Einval);
    }

    let object = if map_anon {
        if !map_private {
            return errno_ret(Errno::Enosys);
        }
        VmObject::new_anon(aligned_len)
    } else {
        let fd_table = task.fd_table.lock();
        let desc = match fd_table.get(fd) {
            Some(desc) => Arc::clone(desc),
            None => return errno_ret(Errno::Ebadf),
        };
        drop(fd_table);

        let vnode = match &desc.object {
            FileObject::Vnode(vnode) => Arc::clone(vnode),
            _ => return errno_ret(Errno::Enodev),
        };

        if vnode.vtype() != VnodeType::Regular {
            return errno_ret(Errno::Enodev);
        }
        if perm.contains(MapPerm::R) && !desc.flags.read {
            return errno_ret(Errno::Ebadf);
        }
        if perm.contains(MapPerm::W) && !desc.flags.write {
            return errno_ret(Errno::Ebadf);
        }

        let _ = aligned_len;
        let _ = offset;
        build_file_backed_object(&vnode, map_private)
    };

    let mut vm = task.vm_map.lock();
    let base = match resolve_mmap_base(&mut vm, addr, aligned_len, map_fixed) {
        Ok(base) => base,
        Err(errno) => return errno_ret(errno),
    };

    let vma = VmMapEntry::new(
        base as u64,
        (base + aligned_len) as u64,
        BackingStore::Object { object, offset },
        EntryFlags::empty(),
        perm,
    );

    match vm.insert_entry(vma) {
        Ok(()) => base,
        Err(_) => errno_ret(Errno::Einval),
    }
}

/// sys_munmap: tear down PTEs + TLB + remove/split VMAs.
pub fn sys_munmap(task: &Arc<Task>, addr: usize, len: usize) -> usize {
    if len == 0 || !is_page_aligned(addr) {
        return errno_ret(Errno::Einval);
    }

    let end = match addr.checked_add(len) {
        Some(end) => end,
        None => return errno_ret(Errno::Einval),
    };

    let aligned_end = match align_up_to_page(end) {
        Some(end) => VirtAddr::new(end),
        None => return errno_ret(Errno::Einval),
    };
    let aligned_start = VirtAddr::new(addr);
    if aligned_start >= aligned_end {
        return errno_ret(Errno::Einval);
    }
    let mut vm = task.vm_map.lock();
    let removed = vm.remove_range(aligned_start, aligned_end);
    free_removed_frames(removed);
    0
}

/// sys_mprotect: change VMA permissions + update PTEs.
pub fn sys_mprotect(task: &Arc<Task>, addr: usize, len: usize, prot_bits: usize) -> usize {
    if len == 0 || !is_page_aligned(addr) {
        return errno_ret(Errno::Einval);
    }

    let end = match addr.checked_add(len) {
        Some(end) => end,
        None => return errno_ret(Errno::Einval),
    };

    let end = match align_up_to_page(end) {
        Some(end) => VirtAddr::new(end),
        None => return errno_ret(Errno::Einval),
    };
    let start = VirtAddr::new(addr);
    if start >= end {
        return errno_ret(Errno::Einval);
    }

    let perm = match prot_bits_to_perm(prot_bits) {
        Ok(perm) => perm,
        Err(errno) => return errno_ret(errno),
    };

    let mut vm = task.vm_map.lock();
    match vm.protect_range(start, end, perm) {
        Ok(()) => 0,
        Err(e) => errno_ret(e),
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
