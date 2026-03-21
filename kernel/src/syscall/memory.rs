//! Memory management system calls.
//!
//! Implements mmap, munmap, brk, mprotect and related memory operations.
//!
//! All functions return [`KernelResult<usize>`] except [`sys_brk`] which
//! has special Linux ABI semantics (returns current brk on failure, never
//! a negative errno).

use alloc::sync::Arc;

use crate::{
    fs::FileObject,
    hal_common::{Errno, KernelResult, VirtAddr, PAGE_SIZE},
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
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "prot_bits_to_perm: unsupported protection bits prot={:#x}",
            prot_bits
        ));
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
            return Err(kerr!(
                syscall,
                debug,
                Errno::Einval,
                "resolve_mmap_base: MAP_FIXED addr not page-aligned addr={:#x}",
                addr
            ));
        }
        let end = addr.checked_add(aligned_len).ok_or_else(|| {
            kerr!(
                syscall,
                debug,
                Errno::Einval,
                "resolve_mmap_base: addr+len overflow addr={:#x} len={:#x}",
                addr,
                aligned_len
            )
        })?;
        let removed = vm.remove_range(VirtAddr::new(addr), VirtAddr::new(end));
        free_removed_frames(removed);
        return Ok(addr);
    }

    if addr != 0 {
        let hint = addr & !(PAGE_SIZE - 1);
        let hint_end = hint.checked_add(aligned_len).ok_or_else(|| {
            kerr!(
                syscall,
                debug,
                Errno::Einval,
                "resolve_mmap_base: hint+len overflow hint={:#x} len={:#x}",
                hint,
                aligned_len
            )
        })?;
        if vm.is_range_free(VirtAddr::new(hint), VirtAddr::new(hint_end)) {
            return Ok(hint);
        }
    }

    vm.find_free_area_topdown(aligned_len)
        .map(|va| va.as_usize())
        .ok_or_else(|| {
            kerr!(
                syscall,
                error,
                Errno::Enomem,
                "resolve_mmap_base: no free area found len={:#x}",
                aligned_len
            )
        })
}

fn build_file_backed_object(
    vnode: &Arc<dyn crate::fs::Vnode>,
    map_private: bool,
) -> Arc<crate::hal_common::LeveledRwLock<crate::mm::vm::VmObject, 3>> {
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
) -> KernelResult<usize> {
    use crate::{
        fs::VnodeType,
        mm::vm::{MapPerm, VmMapping, VmObject},
    };

    let aligned_len = match align_up_to_page(len) {
        Some(aligned_len) if aligned_len != 0 => aligned_len,
        _ => {
            return Err(kerr!(
                syscall,
                debug,
                Errno::Einval,
                "sys_mmap: invalid or zero len len={:#x}",
                len
            ))
        },
    };

    if flags & !SUPPORTED_MMAP_FLAGS != 0 {
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_mmap: unsupported flags flags={:#x}",
            flags
        ));
    }

    let map_type = flags & MAP_TYPE_MASK;
    let map_shared = map_type == MAP_SHARED;
    let map_private = map_type == MAP_PRIVATE;
    if !map_shared && !map_private {
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_mmap: neither MAP_SHARED nor MAP_PRIVATE flags={:#x}",
            flags
        ));
    }

    let map_fixed = flags & MAP_FIXED != 0;
    let map_anon = flags & MAP_ANONYMOUS != 0;

    let perm: MapPerm = prot_bits_to_perm(prot_bits)?;

    if offset % PAGE_SIZE as u64 != 0 {
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_mmap: offset not page-aligned offset={:#x}",
            offset
        ));
    }

    let object = if map_anon {
        if !map_private {
            return Err(kerr!(
                syscall,
                debug,
                Errno::Enosys,
                "sys_mmap: MAP_SHARED+MAP_ANONYMOUS not supported"
            ));
        }
        VmObject::new_anon(aligned_len)
    } else {
        let fd_table = task.fd_table.lock();
        let desc = match fd_table.get(fd) {
            Some(desc) => Arc::clone(desc),
            None => {
                return Err(kerr!(
                    syscall,
                    debug,
                    Errno::Ebadf,
                    "sys_mmap: fd not found fd={}",
                    fd
                ))
            },
        };
        drop(fd_table);

        let vnode = match &desc.object {
            FileObject::Vnode(vnode) => Arc::clone(vnode),
            _ => {
                return Err(kerr!(
                    syscall,
                    debug,
                    Errno::Enodev,
                    "sys_mmap: fd is not a vnode fd={}",
                    fd
                ))
            },
        };

        if vnode.vtype() != VnodeType::Regular {
            return Err(kerr!(
                syscall,
                debug,
                Errno::Enodev,
                "sys_mmap: vnode is not a regular file fd={}",
                fd
            ));
        }
        if perm.contains(MapPerm::R) && !desc.flags.read {
            return Err(kerr!(
                syscall,
                debug,
                Errno::Ebadf,
                "sys_mmap: PROT_READ but fd not open for read fd={}",
                fd
            ));
        }
        if perm.contains(MapPerm::W) && !desc.flags.write {
            return Err(kerr!(
                syscall,
                debug,
                Errno::Ebadf,
                "sys_mmap: PROT_WRITE but fd not open for write fd={}",
                fd
            ));
        }

        let _ = aligned_len;
        let _ = offset;
        build_file_backed_object(&vnode, map_private)
    };

    let mut vm = task.vm_map.write();
    let base = resolve_mmap_base(&mut vm, addr, aligned_len, map_fixed)?;

    let mapping = if map_anon {
        VmMapping::AnonPrivate { object, offset }
    } else if map_shared {
        VmMapping::FileShared { object, offset }
    } else {
        VmMapping::FilePrivate { object, offset }
    };

    match vm.map(
        VirtAddr::new(base),
        VirtAddr::new(base + aligned_len),
        perm,
        mapping,
    ) {
        Ok(()) => {
            // Linux allows mprotect to raise permissions within mapping policy
            // (e.g. pthread stack: mmap(PROT_NONE) then mprotect(RW) subrange).
            // Keep max_protection broad for mmap-created regions.
            if let Some(vma) = vm.lookup_mut(VirtAddr::new(base)) {
                vma.max_protection = crate::map_perm!(R, W, X, U);
            }
            Ok(base)
        },
        Err(_) => Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_mmap: map failed base={:#x} len={:#x}",
            base,
            aligned_len
        )),
    }
}

/// sys_munmap: tear down PTEs + TLB + remove/split VMAs.
pub fn sys_munmap(task: &Arc<Task>, addr: usize, len: usize) -> KernelResult<usize> {
    if len == 0 || !is_page_aligned(addr) {
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_munmap: invalid args addr={:#x} len={:#x}",
            addr,
            len
        ));
    }

    let end = addr.checked_add(len).ok_or_else(|| {
        kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_munmap: addr+len overflow addr={:#x} len={:#x}",
            addr,
            len
        )
    })?;

    let aligned_end = match align_up_to_page(end) {
        Some(end) => VirtAddr::new(end),
        None => {
            return Err(kerr!(
                syscall,
                debug,
                Errno::Einval,
                "sys_munmap: align_up_to_page failed end={:#x}",
                end
            ))
        },
    };
    let aligned_start = VirtAddr::new(addr);
    if aligned_start >= aligned_end {
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_munmap: start >= end after alignment start={:#x} end={:#x}",
            aligned_start.as_usize(),
            aligned_end.as_usize()
        ));
    }
    let mut vm = task.vm_map.write();
    let removed = vm.remove_range(aligned_start, aligned_end);
    free_removed_frames(removed);
    Ok(0)
}

/// sys_mprotect: change VMA permissions + update PTEs.
pub fn sys_mprotect(
    task: &Arc<Task>,
    addr: usize,
    len: usize,
    prot_bits: usize,
) -> KernelResult<usize> {
    if len == 0 || !is_page_aligned(addr) {
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_mprotect: invalid args addr={:#x} len={:#x}",
            addr,
            len
        ));
    }

    let end = addr.checked_add(len).ok_or_else(|| {
        kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_mprotect: addr+len overflow addr={:#x} len={:#x}",
            addr,
            len
        )
    })?;

    let end = match align_up_to_page(end) {
        Some(end) => VirtAddr::new(end),
        None => {
            return Err(kerr!(
                syscall,
                debug,
                Errno::Einval,
                "sys_mprotect: align_up_to_page failed end={:#x}",
                end
            ))
        },
    };
    let start = VirtAddr::new(addr);
    if start >= end {
        return Err(kerr!(
            syscall,
            debug,
            Errno::Einval,
            "sys_mprotect: start >= end after alignment start={:#x} end={:#x}",
            start.as_usize(),
            end.as_usize()
        ));
    }

    let perm = prot_bits_to_perm(prot_bits)?;

    let mut vm = task.vm_map.write();
    match vm.protect_range(start, end, perm) {
        Ok(()) => Ok(0),
        Err(e) => Err(e),
    }
}

/// sys_brk: change program break (heap end).
///
/// **Note:** brk has special Linux ABI semantics — it returns the current
/// (or new) break address directly, never a negative errno.  On failure it
/// returns the *unchanged* current break.  This is why it returns a plain
/// `usize` instead of `KernelResult<usize>`.
pub fn sys_brk(task: &Arc<Task>, addr: usize) -> usize {
    use core::sync::atomic::Ordering;

    if addr == 0 {
        return task.brk.load(Ordering::Relaxed);
    }

    let mut vm = task.vm_map.write();
    // Read brk under lock to prevent race with concurrent sys_brk
    let current_brk = task.brk.load(Ordering::Relaxed);
    let requested_brk = addr;
    let new_brk_aligned = (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let old_brk_aligned = (current_brk + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    if new_brk_aligned == old_brk_aligned {
        task.brk.store(requested_brk, Ordering::Relaxed);
        return requested_brk;
    }

    if new_brk_aligned > old_brk_aligned {
        if vm
            .grow_heap(
                VirtAddr::new(old_brk_aligned),
                VirtAddr::new(new_brk_aligned),
            )
            .is_err()
        {
            return current_brk;
        }
    } else if new_brk_aligned < old_brk_aligned {
        match vm.shrink_heap(
            VirtAddr::new(old_brk_aligned),
            VirtAddr::new(new_brk_aligned),
        ) {
            Ok(removed) => free_removed_frames(removed),
            Err(_) => return current_brk,
        }
    }

    task.brk.store(requested_brk, Ordering::Relaxed);
    requested_brk
}
