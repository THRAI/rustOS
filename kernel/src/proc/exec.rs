//! ELF loading and exec() implementation.
//!
//! do_execve() is the unified exec entry point with two-phase commit semantics:
//!   Stage 1: Parse and validate ELF (can fail, no state change)
//!   Stage 2: Build new address space in temporary VmMap (can fail, no state change)
//!   Stage 3: Point of no return -- atomic swap of old address space with new
//!
//! All page allocation goes through VmObject methods (fetch_page_async,
//! fault_allocate_anon, new_vnode_region) — never the allocator directly.
//! ELF file pages are fetched via VmObject's pager interface.
//!
//! The old exec()/exec_with_args() functions are preserved behind the
//! "exec-legacy" feature flag for debugging.

use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};

use crate::{
    fs::{resolve, vnode_object, VnodeType},
    hal_common::{Errno, VirtAddr, PAGE_SIZE},
    mm::{
        pmap_activate, pmap_create, pmap_destroy, pmap_enter, BackingStore, EntryFlags, MapPerm,
        VmMap, VmMapEntry, VmObject,
    },
    proc::{map_sigcode_page, parse_elf_first_page, ExecContext, SigSet, Task, SIG_DFL, SIG_IGN},
};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

/// Dynamic linker load offset (reference: Chronix 0x20_0000_0000).
const DL_INTERP_OFFSET: usize = 0x20_0000_0000;

/// User stack size: 64KB.
const USER_STACK_SIZE: usize = 64 * 1024;
/// User stack top address (just below kernel space).
const USER_STACK_TOP: usize = 0x0000_003F_FFFF_F000;

// ---------------------------------------------------------------------------
// Auxiliary vector types (Linux ABI)
// ---------------------------------------------------------------------------

const AT_NULL: usize = 0;
const AT_PHDR: usize = 3;
const AT_PHENT: usize = 4;
const AT_PHNUM: usize = 5;
const AT_PAGESZ: usize = 6;
const AT_ENTRY: usize = 9;
const AT_RANDOM: usize = 25;

// ---------------------------------------------------------------------------
// VMA insert helper
// ---------------------------------------------------------------------------

/// Map a VM insert error to an exec-appropriate errno.
/// During exec, overlap/invalid-range means the ELF binary is malformed.
fn map_insert_err(_err: Errno) -> Errno {
    Errno::Enoexec
}

// ---------------------------------------------------------------------------
// do_execve: Unified exec with two-phase commit
// ---------------------------------------------------------------------------

/// Unified exec entry point with BSD two-phase commit semantics.
///
/// A failed exec at any stage before point-of-no-return leaves the calling
/// process's vm_map and page table completely intact.
///
/// Returns (entry_point, stack_pointer) on success.
pub async fn do_execve(
    task: &Arc<Task>,
    elf_path: &str,
    argv: &[String],
    envp: &[String],
) -> Result<(usize, usize), Errno> {
    klog!(exec, debug, "do_execve pid={} path={}", task.pid, elf_path);

    // =====================================================================
    // STAGE 1: Parse and validate (can fail, no state change)
    // =====================================================================

    // 1a. Resolve path to vnode
    let vnode = resolve(elf_path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(Errno::Enoexec);
    }

    // 1b. Create a file-backed VmObject for the ELF vnode and fetch page 0
    //     through the pager (not the allocator directly).
    let elf_obj = VmObject::new_file(&*vnode);
    VmObject::fetch_page_async(Arc::clone(&elf_obj), crate::mm::vm::VObjIndex::new(0))
        .await
        .map_err(|_| Errno::Enoexec)?;

    // 1c. Parse ELF header + program headers via goblin
    let hdr_phys = elf_obj
        .read()
        .lookup_page(crate::mm::vm::VObjIndex::new(0))
        .ok_or(Errno::Enoexec)?;

    let hdr_buf = hdr_phys.as_slice();

    let (ehdr, phdrs) = match parse_elf_first_page(hdr_buf) {
        Ok(parsed) => parsed,
        Err(_) => return Err(Errno::Enoexec),
    };

    // 1d. Build ExecContext (pure function, no side effects)
    let load_base = VirtAddr(0); // ET_EXEC: no bias; TODO: PIE support
    let exec_ctx = ExecContext::build(hdr_buf, &ehdr, &phdrs, load_base);

    // =====================================================================
    // STAGE 2: Build new address space in temporary VmMap (can fail, no
    //          state change to the calling process)
    // =====================================================================

    // 2a. Create a fresh pmap and VmMap
    let new_pmap = pmap_create();
    let new_pmap_arc = Arc::new(crate::hal_common::SpinMutex::new(new_pmap));
    let mut new_vm = VmMap::new(Arc::clone(&new_pmap_arc));

    // Some ELF binaries split one page across multiple PT_LOAD segments.
    // Keep the maximum file-backed coverage per merged page to preserve
    // correct zero-fill semantics for BSS tails.
    let mut merged_file_bytes: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for region in &exec_ctx.mapped_regions {
        if region.memsz == 0 {
            continue;
        }
        let va_start = region.vaddr & !(PAGE_SIZE - 1);
        let file_offset_page_aligned = region.offset & !(PAGE_SIZE - 1);
        let file_prefix = region.offset.saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + region.filesz;
        let key = (va_start, file_offset_page_aligned);
        let cur = merged_file_bytes.get(&key).copied().unwrap_or(0);
        if file_backed_bytes > cur {
            merged_file_bytes.insert(key, file_backed_bytes);
        }
    }

    // 2b. Create VMAs for each PT_LOAD region from ExecContext
    for region in &exec_ctx.mapped_regions {
        let va_start = region.vaddr & !(PAGE_SIZE - 1);
        let va_end_raw = region.vaddr + region.memsz;
        let va_end = (va_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let file_offset_page_aligned = region.offset & !(PAGE_SIZE - 1);
        let file_prefix = region.offset.saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + region.filesz;
        let merged_backed_bytes = merged_file_bytes
            .get(&(va_start, file_offset_page_aligned))
            .copied()
            .unwrap_or(file_backed_bytes);

        let obj_size = (va_end - va_start) / PAGE_SIZE;
        let obj = VmObject::new_vnode_region(
            vnode.vnode_id() as usize,
            vnode.path(),
            obj_size,
            file_offset_page_aligned,
            merged_backed_bytes,
        );

        let vma = VmMapEntry::new_file_backed(
            va_start as u64,
            va_end as u64,
            BackingStore::Object {
                object: obj,
                offset: file_offset_page_aligned as u64,
            },
            EntryFlags::empty(),
            region.prot,
        );

        if let Err(e) = insert_or_merge_file_vma(&mut new_vm, vma) {
            // new_vm drops automatically, cleaning up
            return Err(map_insert_err(e));
        }
    }

    // 2c. If there is an interpreter (PT_INTERP), load it into the new VmMap
    let final_entry = if let Some(ref ipath) = exec_ctx.interp_path {
        klog!(
            exec,
            debug,
            "do_execve pid={} loading interp={}",
            task.pid,
            ipath
        );
        match load_interp_into(&mut new_vm, ipath, DL_INTERP_OFFSET).await {
            Ok(interp_entry) => {
                klog!(
                    exec,
                    debug,
                    "do_execve pid={} interp entry={:#x}",
                    task.pid,
                    interp_entry
                );
                interp_entry
            },
            Err(e) => {
                klog!(
                    exec,
                    error,
                    "do_execve pid={} failed to load interp {}: {:?}",
                    task.pid,
                    ipath,
                    e
                );
                return Err(e);
            },
        }
    } else {
        exec_ctx.user_entry.0
    };

    // 2d. Create user stack VMA (anonymous, RW)
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let stack_obj = VmObject::new_anon(USER_STACK_SIZE);
    let stack_obj_ref: Arc<crate::hal_common::LeveledRwLock<VmObject, 3>> = Arc::clone(&stack_obj);
    let stack_vma = VmMapEntry::new(
        stack_bottom as u64,
        USER_STACK_TOP as u64,
        BackingStore::Object {
            object: stack_obj,
            offset: 0,
        },
        EntryFlags::empty(),
        crate::map_perm!(R, W, U),
    );
    if new_vm.insert_entry(stack_vma).is_err() {
        return Err(Errno::Enomem);
    }

    // 2e. Set up argv/envp/auxv on top stack page
    let (sp_va, entry_for_auxv) = {
        // Eagerly allocate the top stack page through the stack VmObject
        let stack_page_va = USER_STACK_TOP - PAGE_SIZE;
        let stack_page_idx = crate::mm::vm::VObjIndex::new((USER_STACK_SIZE / PAGE_SIZE) - 1);

        // Allocate and zero the page via VmObject (not the allocator directly)
        let phys = {
            let mut obj = stack_obj_ref.write();
            obj.fault_allocate_anon(stack_page_idx)
                .map_err(|_| Errno::Enomem)?
        };

        // Map in new pmap
        {
            let mut pmap = new_pmap_arc.lock();
            let _ = pmap_enter(
                &mut pmap,
                VirtAddr::new(stack_page_va),
                phys,
                crate::map_perm!(R, W, U),
                false,
            );
        }

        // Write stack layout into the physical frame (identity-mapped)
        let mut cursor = crate::hal_common::PageCursor::new(phys, PAGE_SIZE).unwrap();
        let stack_page_vbase = VirtAddr::new(stack_page_va);

        // Push 16 random bytes for AT_RANDOM
        let random_va = {
            let slice = cursor.alloc_down_bytes(16).unwrap();
            slice.fill(0); // Zero for now (deterministic but functional)
            cursor.current_va(stack_page_vbase)
        };

        // Push all strings and record their VAs
        let mut argv_vas: Vec<usize> = Vec::with_capacity(argv.len());
        for arg in argv {
            let s = arg.as_bytes();
            let len = s.len() + 1; // include NUL
            let slice = cursor.alloc_down_bytes(len).unwrap();
            slice[..s.len()].copy_from_slice(s);
            slice[s.len()] = 0;
            argv_vas.push(cursor.current_va(stack_page_vbase).as_usize());
        }
        let mut envp_vas: Vec<usize> = Vec::with_capacity(envp.len());
        for env in envp {
            let s = env.as_bytes();
            let len = s.len() + 1;
            let slice = cursor.alloc_down_bytes(len).unwrap();
            slice[..s.len()].copy_from_slice(s);
            slice[s.len()] = 0;
            envp_vas.push(cursor.current_va(stack_page_vbase).as_usize());
        }

        // Build auxv entries
        let auxv: [(usize, usize); 4] = [
            (AT_PAGESZ, PAGE_SIZE),
            (AT_ENTRY, final_entry),
            (AT_RANDOM, random_va.as_usize()),
            (AT_NULL, 0),
        ];

        let n_slots = 1 + argv.len() + 1 + envp.len() + 1 + auxv.len() * 2;
        let slots_bytes = n_slots * core::mem::size_of::<usize>();

        cursor.align_down(16);

        let prev_offset = cursor.current_offset();
        if prev_offset < slots_bytes {
            return Err(Errno::Enomem);
        }
        let new_offset = prev_offset - slots_bytes;
        let sp = stack_page_vbase.as_usize() + new_offset;

        let slice_ptr: *mut u8;
        {
            let allocated = cursor.alloc_down_bytes(slots_bytes).ok_or(Errno::Enomem)?;
            slice_ptr = allocated.as_mut_ptr();
        }
        let slice = unsafe { core::slice::from_raw_parts_mut(slice_ptr, slots_bytes) };

        let mut off = 0;
        let mut write_usize = |val: usize| {
            slice[off..off + core::mem::size_of::<usize>()].copy_from_slice(&val.to_ne_bytes());
            off += core::mem::size_of::<usize>();
        };

        // argc
        write_usize(argv.len());
        for va in &argv_vas {
            write_usize(*va);
        }
        write_usize(0); // NULL

        for va in &envp_vas {
            write_usize(*va);
        }
        write_usize(0); // NULL

        for &(atype, aval) in &auxv {
            write_usize(atype);
            write_usize(aval);
        }

        (sp, final_entry)
    };

    // 2f. Map sigcode trampoline page in the new pmap
    {
        let mut pmap = new_pmap_arc.lock();
        map_sigcode_page(&mut pmap);
    }

    // =====================================================================
    // STAGE 3: Point of no return -- atomic swap
    // =====================================================================

    // 3a. Swap old VmMap with new VmMap
    {
        let mut vm = task.vm_map.lock();
        // Replace the old VmMap; old one drops automatically
        let old_vm = core::mem::replace(&mut *vm, new_vm);
        // Destroy old pmap: switch satp first, then free old page tables
        let mut new_pmap_guard = vm.pmap_lock();
        pmap_activate(&mut new_pmap_guard);
        drop(new_pmap_guard);
        // Now satp points to new root -- safe to destroy old
        let mut old_pmap_guard = old_vm.pmap_lock();
        pmap_destroy(&mut old_pmap_guard);
        // old_vm + old_pmap drop when guards/variables go out of scope
    }

    // 3b. Update task brk
    task.brk
        .store(exec_ctx.brk.0, core::sync::atomic::Ordering::Relaxed);

    // 3c. Reset signal dispositions (POSIX: exec resets caught signals to SIG_DFL)
    {
        let mut actions = task.signals.actions.lock();
        for act in actions.iter_mut() {
            if act.handler != SIG_DFL && act.handler != SIG_IGN {
                act.handler = SIG_DFL;
                act.flags = 0;
                act.mask = 0;
            }
        }
    }
    // Clear pending signals on exec
    task.signals
        .pending
        .store(SigSet(0), core::sync::atomic::Ordering::Relaxed);
    // Clear blocked signal mask
    task.signals
        .blocked
        .store(SigSet(0), core::sync::atomic::Ordering::Relaxed);

    // 3d. Close O_CLOEXEC file descriptors
    task.fd_table.lock().strip_cloexec();

    klog!(
        exec,
        debug,
        "do_execve pid={} entry={:#x} sp={:#x}",
        task.pid,
        entry_for_auxv,
        sp_va
    );
    Ok((entry_for_auxv, sp_va))
}

// ---------------------------------------------------------------------------
// load_interp_into: load dynamic linker into a VmMap (not the task's)
// ---------------------------------------------------------------------------

/// Load an interpreter ELF into the given VmMap at the specified offset.
/// Used by do_execve to load into the temporary VmMap before swap.
///
/// Pages are fetched through VmObject's pager interface, not the allocator.
async fn load_interp_into(
    vm: &mut VmMap,
    interp_path: &str,
    offset: usize,
) -> Result<usize, Errno> {
    let vnode = resolve(interp_path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(Errno::Enoexec);
    }

    // Fetch the interpreter's first page through a VmObject
    let interp_obj = VmObject::new_file(&*vnode);
    VmObject::fetch_page_async(Arc::clone(&interp_obj), crate::mm::vm::VObjIndex::new(0))
        .await
        .map_err(|_| Errno::Enoexec)?;

    let hdr_phys = interp_obj
        .read()
        .lookup_page(crate::mm::vm::VObjIndex::new(0))
        .ok_or(Errno::Enoexec)?;
    let hdr_buf = hdr_phys.as_slice();

    // Use the hand-rolled parser for interp (same as legacy path, works fine)
    let elf_hdr = parse_elf_header(hdr_buf)?;
    let phdrs = parse_phdrs(hdr_buf, elf_hdr)?;
    let interp_entry = elf_hdr.e_entry as usize + offset;

    let mut merged_file_bytes: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for phdr in phdrs.iter() {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }
        let va_start = ((phdr.p_vaddr as usize) + offset) & !(PAGE_SIZE - 1);
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);
        let file_prefix = (phdr.p_offset as usize).saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + phdr.p_filesz as usize;
        let key = (va_start, file_offset_page_aligned);
        let cur = merged_file_bytes.get(&key).copied().unwrap_or(0);
        if file_backed_bytes > cur {
            merged_file_bytes.insert(key, file_backed_bytes);
        }
    }

    for phdr in phdrs {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }

        let va_start = ((phdr.p_vaddr as usize) + offset) & !(PAGE_SIZE - 1);
        let va_end_raw = phdr.p_vaddr as usize + offset + phdr.p_memsz as usize;
        let va_end = (va_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let prot = elf_flags_to_prot(phdr.p_flags);
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);
        let file_prefix = (phdr.p_offset as usize).saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + phdr.p_filesz as usize;
        let merged_backed_bytes = merged_file_bytes
            .get(&(va_start, file_offset_page_aligned))
            .copied()
            .unwrap_or(file_backed_bytes);

        let obj_size = (va_end - va_start) / PAGE_SIZE;
        let obj = VmObject::new_vnode_region(
            vnode.vnode_id() as usize,
            vnode.path(),
            obj_size,
            file_offset_page_aligned,
            merged_backed_bytes,
        );
        let vma = VmMapEntry::new(
            va_start as u64,
            va_end as u64,
            BackingStore::Object {
                object: obj,
                offset: file_offset_page_aligned as u64,
            },
            EntryFlags::empty(),
            prot,
        );

        if let Err(e) = insert_or_merge_file_vma(vm, vma) {
            return Err(map_insert_err(e));
        }
    }

    Ok(interp_entry)
}

// ===========================================================================
// Legacy exec code (gated behind feature flag)
// ===========================================================================

#[cfg(feature = "exec-legacy")]
mod legacy {

    use alloc::{string::String, sync::Arc, vec::Vec};

    use super::{
        elf_flags_to_prot, insert_or_merge_file_vma, load_interp, map_insert_err, parse_elf_header,
        parse_phdrs, Elf64Header, AT_ENTRY, AT_NULL, AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM,
        AT_RANDOM, DL_INTERP_OFFSET, PT_INTERP, PT_LOAD, USER_STACK_SIZE, USER_STACK_TOP,
    };
    use crate::{
        fs::{resolve, vnode_object, VnodeType},
        hal_common::{Errno, VirtAddr, PAGE_SIZE},
        mm::{
            pmap_activate, pmap_create, pmap_destroy, pmap_enter, BackingStore, EntryFlags,
            MapPerm, VmMap, VmMapEntry, VmObject,
        },
        proc::Task,
    };

    /// Execute an ELF binary: resolve path, parse ELF, reset vm_map, create
    /// demand-paged VMAs, set up user stack. Returns (entry_point, stack_pointer).
    pub async fn exec(task: &Arc<Task>, elf_path: &str) -> Result<(usize, usize), Errno> {
        klog!(exec, debug, "exec pid={} path={}", task.pid, elf_path);
        let vnode = resolve(elf_path).await?;
        if vnode.vtype() != VnodeType::Regular {
            return Err(Errno::Enoexec);
        }

        let file_size = vnode.size();
        if file_size < core::mem::size_of::<Elf64Header>() as u64 {
            return Err(Errno::Enoexec);
        }

        let obj = vnode_object(&*vnode);
        let page0 = VmObject::fetch_page_async(obj, crate::hal_common::PageNum::new(0))
            .await
            .map_err(|_| Errno::Enoexec)?;

        let hdr_buf = unsafe {
            core::slice::from_raw_parts(page0.phys_addr.as_usize() as *const u8, PAGE_SIZE)
        };
        let elf_hdr = parse_elf_header(hdr_buf)?;
        let phdrs = parse_phdrs(hdr_buf, elf_hdr)?;

        let load_bias: usize = 0x0;
        let entry = elf_hdr.e_entry as usize + load_bias;

        let mut interp_path: Option<alloc::string::String> = None;
        for phdr in phdrs {
            if phdr.p_type == PT_INTERP {
                let off = phdr.p_offset as usize;
                let sz = phdr.p_filesz as usize;
                if off + sz <= PAGE_SIZE {
                    let bytes = &hdr_buf[off..off + sz];
                    let s = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
                    if let Ok(p) = core::str::from_utf8(s) {
                        interp_path = Some(alloc::string::String::from(p));
                    }
                }
                break;
            }
        }

        {
            let mut vm = task.vm_map.lock();
            vm.clear();
            let mut pmap = vm.pmap_lock();
            let mut old_pmap = core::mem::replace(&mut *pmap, pmap_create());
            pmap_activate(&mut pmap);
            pmap_destroy(&mut old_pmap);
        }

        let mut brk_end: usize = 0;
        for phdr in phdrs {
            if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
                continue;
            }

            let va_start = ((phdr.p_vaddr as usize) + load_bias) & !(PAGE_SIZE - 1);
            let va_end_raw = phdr.p_vaddr as usize + load_bias + phdr.p_memsz as usize;
            let va_end = (va_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

            let prot = elf_flags_to_prot(phdr.p_flags);
            let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);

            let obj_size = va_end - va_start;
            let obj = VmObject::new_anon(obj_size);
            obj.write().pager = Some(Arc::new(crate::mm::vm::VnodePager {
                vnode_id: vnode.vnode_id() as usize,
                path: alloc::string::String::from(vnode.path()),
            }));

            let vma = VmMapEntry::new(
                va_start as u64,
                va_end as u64,
                BackingStore::Object {
                    object: obj,
                    offset: file_offset_page_aligned as u64,
                },
                EntryFlags::empty(),
                prot,
            );

            let mut vm = task.vm_map.lock();
            if let Err(e) = insert_or_merge_file_vma(&mut vm, vma) {
                return Err(map_insert_err(e));
            }

            if va_end > brk_end {
                brk_end = va_end;
            }
        }

        task.brk
            .store(brk_end, core::sync::atomic::Ordering::Relaxed);

        let final_entry = if let Some(ref ipath) = interp_path {
            klog!(
                exec,
                debug,
                "exec pid={} loading interp={}",
                task.pid,
                ipath
            );
            match load_interp(task, ipath, DL_INTERP_OFFSET).await {
                Ok(interp_entry) => {
                    klog!(
                        exec,
                        debug,
                        "exec pid={} interp entry={:#x}",
                        task.pid,
                        interp_entry
                    );
                    interp_entry
                },
                Err(e) => {
                    klog!(
                        exec,
                        error,
                        "exec pid={} failed to load interp {}: {:?}",
                        task.pid,
                        ipath,
                        e
                    );
                    return Err(e);
                },
            }
        } else {
            entry
        };

        let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
        let stack_obj = VmObject::new_anon(USER_STACK_SIZE);
        let stack_vma = VmMapEntry::new(
            stack_bottom as u64,
            USER_STACK_TOP as u64,
            BackingStore::Object {
                object: stack_obj,
                offset: 0,
            },
            EntryFlags::empty(),
            crate::map_perm!(R, W, U),
        );
        {
            let mut vm = task.vm_map.lock();
            if vm.insert_entry(stack_vma).is_err() {
                return Err(Errno::Enomem);
            }
        }

        task.fd_table.lock().strip_cloexec();

        {
            let vm_map = task.vm_map.lock();
            let mut pmap = vm_map.pmap_lock();
            crate::proc::map_sigcode_page(&mut pmap);
        }

        {
            let mut actions = task.signals.actions.lock();
            for act in actions.iter_mut() {
                if act.handler != crate::proc::SIG_DFL && act.handler != crate::proc::SIG_IGN {
                    act.handler = crate::proc::SIG_DFL;
                    act.flags = 0;
                    act.mask = 0;
                }
            }
        }
        task.signals.pending.store(
            crate::proc::SigSet(0),
            core::sync::atomic::Ordering::Relaxed,
        );
        task.signals.blocked.store(
            crate::proc::SigSet(0),
            core::sync::atomic::Ordering::Relaxed,
        );

        klog!(
            exec,
            debug,
            "exec pid={} entry={:#x} sp={:#x}",
            task.pid,
            final_entry,
            USER_STACK_TOP
        );
        Ok((final_entry, USER_STACK_TOP))
    }

    /// Execute an ELF binary with argv/envp.
    pub async fn exec_with_args(
        task: &Arc<Task>,
        elf_path: &str,
        argv: &[String],
        envp: &[String],
    ) -> Result<(usize, usize), Errno> {
        let (entry, _) = exec(task, elf_path).await?;

        let stack_page_va = USER_STACK_TOP - PAGE_SIZE;
        let vm_page = crate::mm::alloc_anon_sync().expect("OOM when allocating user stack");
        let phys = vm_page.phys();
        pmap_zero_page(phys);

        {
            let mut vm = task.vm_map.lock();
            if let Some(vma) = vm.lookup_mut(stack_page_va as u64) {
                let page_idx = crate::hal_common::VirtPageNum(
                    (stack_page_va - vma.start as usize) / PAGE_SIZE
                        + match &vma.store {
                            BackingStore::Object { offset, .. } => *offset as usize / PAGE_SIZE,
                            _ => 0,
                        },
                );
                if let BackingStore::Object { object, .. } = &vma.store {
                    let mut obj = object.write();
                    let mut page = crate::mm::vm::VmPage::new();
                    page.phys_addr = phys;
                    obj.insert_page(page_idx, Arc::new(page));
                }
            } else {
                return Err(Errno::Enomem);
            }

            let mut pmap = vm.pmap_lock();
            let _ = pmap_enter(
                &mut pmap,
                VirtAddr::new(stack_page_va),
                phys,
                crate::map_perm!(R, W, U),
                false,
            );
        }

        let mut cursor = crate::hal_common::PageCursor::new(phys, PAGE_SIZE).unwrap();
        let stack_page_vbase = VirtAddr::new(stack_page_va);

        let random_va = {
            let slice = cursor.alloc_down_bytes(16).unwrap();
            slice.fill(0);
            cursor.current_va(stack_page_vbase)
        };

        let mut argv_vas: Vec<usize> = Vec::with_capacity(argv.len());
        for arg in argv {
            let s = arg.as_bytes();
            let len = s.len() + 1;
            let slice = cursor.alloc_down_bytes(len).unwrap();
            slice[..s.len()].copy_from_slice(s);
            slice[s.len()] = 0;
            argv_vas.push(cursor.current_va(stack_page_vbase).as_usize());
        }
        let mut envp_vas: Vec<usize> = Vec::with_capacity(envp.len());
        for env in envp {
            let s = env.as_bytes();
            let len = s.len() + 1;
            let slice = cursor.alloc_down_bytes(len).unwrap();
            slice[..s.len()].copy_from_slice(s);
            slice[s.len()] = 0;
            envp_vas.push(cursor.current_va(stack_page_vbase).as_usize());
        }

        let auxv: [(usize, usize); 4] = [
            (AT_PAGESZ, PAGE_SIZE),
            (AT_ENTRY, entry),
            (AT_RANDOM, random_va.as_usize()),
            (AT_NULL, 0),
        ];

        let n_slots = 1 + argv.len() + 1 + envp.len() + 1 + auxv.len() * 2;
        let slots_bytes = n_slots * core::mem::size_of::<usize>();

        cursor.align_down(16);

        let prev_offset = cursor.current_offset();
        if prev_offset < slots_bytes {
            return Err(Errno::Enomem);
        }
        let new_offset = prev_offset - slots_bytes;
        let sp_va = stack_page_vbase.as_usize() + new_offset;

        let slice_ptr: *mut u8;
        {
            let allocated = cursor.alloc_down_bytes(slots_bytes).ok_or(Errno::Enomem)?;
            slice_ptr = allocated.as_mut_ptr();
        }
        let slice = unsafe { core::slice::from_raw_parts_mut(slice_ptr, slots_bytes) };

        let mut offset = 0;
        let mut write_usize = |val: usize| {
            slice[offset..offset + core::mem::size_of::<usize>()]
                .copy_from_slice(&val.to_ne_bytes());
            offset += core::mem::size_of::<usize>();
        };

        write_usize(argv.len());
        for va in &argv_vas {
            write_usize(*va);
        }
        write_usize(0);

        for va in &envp_vas {
            write_usize(*va);
        }
        write_usize(0);

        for &(atype, aval) in &auxv {
            write_usize(atype);
            write_usize(aval);
        }

        Ok((entry, sp_va))
    }
} // mod legacy

// Re-export legacy functions when feature is enabled
#[cfg(feature = "exec-legacy")]
pub use legacy::{exec, exec_with_args};

// ---------------------------------------------------------------------------
// Hand-rolled ELF64 structures and parsers (used by load_interp_into)
// ---------------------------------------------------------------------------

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_RISCV: u16 = 243;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Header {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

fn parse_elf_header(buf: &[u8]) -> Result<&Elf64Header, Errno> {
    if buf.len() < core::mem::size_of::<Elf64Header>() {
        return Err(Errno::Enoexec);
    }
    let hdr = unsafe { &*(buf.as_ptr() as *const Elf64Header) };
    if hdr.e_ident[0..4] != ELF_MAGIC {
        return Err(Errno::Enoexec);
    }
    if hdr.e_ident[4] != ELFCLASS64 || hdr.e_ident[5] != ELFDATA2LSB {
        return Err(Errno::Enoexec);
    }
    if (hdr.e_type != ET_EXEC && hdr.e_type != ET_DYN) || hdr.e_machine != EM_RISCV {
        return Err(Errno::Enoexec);
    }
    Ok(hdr)
}

fn parse_phdrs<'a>(buf: &'a [u8], hdr: &Elf64Header) -> Result<&'a [Elf64Phdr], Errno> {
    let off = hdr.e_phoff as usize;
    let num = hdr.e_phnum as usize;
    let ent = hdr.e_phentsize as usize;
    let end = off + num * ent;
    if end > buf.len() || ent < core::mem::size_of::<Elf64Phdr>() {
        return Err(Errno::Enoexec);
    }
    let ptr = unsafe { buf.as_ptr().add(off) as *const Elf64Phdr };
    Ok(unsafe { core::slice::from_raw_parts(ptr, num) })
}

fn elf_flags_to_prot(flags: u32) -> MapPerm {
    let mut prot = MapPerm::U;
    if flags & PF_R != 0 {
        prot |= MapPerm::R;
    }
    if flags & PF_W != 0 {
        prot |= MapPerm::W;
    }
    if flags & PF_X != 0 {
        prot |= MapPerm::X;
    }
    prot
}

/// Insert a file-backed VMA, merging exact same-page overlaps produced by
/// ELF PT_LOAD headers that split permissions inside one page.
///
/// During exec each PT_LOAD creates a fresh VmObject, so we cannot rely on
/// Arc identity (Arc::ptr_eq) to detect same-file overlaps.  Instead we
/// match on identical file offset — sufficient because this function is
/// only called during exec where all segments originate from the same
/// vnode.
fn insert_or_merge_file_vma(vm: &mut VmMap, new_vma: VmMapEntry) -> Result<(), Errno> {
    if let BackingStore::Object { .. } = new_vma.store {
        if let Some(existing) = vm.lookup_mut(new_vma.start()) {
            if existing.start() == new_vma.start() && existing.end() == new_vma.end() {
                let same_backing = match (&existing.store, &new_vma.store) {
                    (
                        BackingStore::Object { offset: f1, .. },
                        BackingStore::Object { offset: f2, .. },
                    ) => f1 == f2,
                    _ => false,
                };
                if same_backing {
                    // Merge segment perms (e.g. text R|X|U + rodata R|U = R|X|U)
                    existing.protection |= new_vma.protection;
                    return Ok(());
                }
            }
        }
    }
    vm.insert_entry(new_vma)
}

// ---------------------------------------------------------------------------
// exec() — legacy non-atomic exec (destructive: clears vm_map before build)
// ---------------------------------------------------------------------------

/// Execute an ELF binary: resolve path, parse ELF, reset vm_map, create
/// demand-paged VMAs, set up user stack. Returns (entry_point, stack_pointer).
///
/// This is an async function because path resolution and ELF header reading
/// go through the delegate channel.
///
/// **WARNING**: Unlike `do_execve()`, this function is NOT two-phase-commit
/// safe — it clears the old address space before building the new one.  A
/// failure after the clear leaves the process in a broken state.  Prefer
/// `do_execve()` for production use.
#[cfg(feature = "exec-legacy")]
pub async fn exec(task: &Arc<Task>, elf_path: &str) -> Result<(usize, usize), Errno> {
    klog!(exec, debug, "exec pid={} path={}", task.pid, elf_path);
    // 1. Resolve path to vnode
    let vnode = resolve(elf_path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(Errno::Enoexec);
    }

    // 2. Read ELF header + program headers (first 4KB is enough)
    let file_size = vnode.size();
    if file_size < core::mem::size_of::<Elf64Header>() as u64 {
        return Err(Errno::Enoexec);
    }

    let obj = vnode_object(&*vnode);
    let page0 = VmObject::fetch_page_async(obj, crate::mm::vm::VObjIndex::new(0))
        .await
        .map_err(|_| Errno::Enoexec)?;

    let hdr_buf =
        unsafe { core::slice::from_raw_parts(page0.phys_addr.as_usize() as *const u8, PAGE_SIZE) };
    let elf_hdr = parse_elf_header(hdr_buf)?;
    let phdrs = parse_phdrs(hdr_buf, elf_hdr)?;

    // PIE: ET_DYN 需要一个加载基址，ET_EXEC 基址为 0
    let load_bias: usize = 0x0;
    let entry = elf_hdr.e_entry as usize + load_bias;

    // 扫描 PT_INTERP，获取动态链接器路径
    let mut interp_path: Option<alloc::string::String> = None;
    for phdr in phdrs {
        if phdr.p_type == PT_INTERP {
            let off = phdr.p_offset as usize;
            let sz = phdr.p_filesz as usize;
            if off + sz <= PAGE_SIZE {
                let bytes = &hdr_buf[off..off + sz];
                // 去掉末尾 NUL
                let s = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
                if let Ok(p) = core::str::from_utf8(s) {
                    interp_path = Some(alloc::string::String::from(p));
                }
            }
            break;
        }
    }

    // 3. Reset vm_map and pmap: tear down old address space
    {
        let mut vm = task.vm_map.lock();
        vm.clear();
        let mut pmap = vm.pmap_lock();
        // Create the new pmap BEFORE destroying the old one.  We must switch
        // satp to the new root page table before freeing the old root,
        // otherwise any TLB miss would walk freed memory (use-after-free on
        // the page table), causing a cascading fault.
        let mut old_pmap = core::mem::replace(&mut *pmap, pmap_create());
        pmap_activate(&mut pmap);
        // Now satp points to the new root — safe to free the old page tables.
        pmap_destroy(&mut old_pmap);
    }

    // Some ELF binaries split one page across multiple PT_LOAD segments
    // (e.g. text + rodata). Keep the maximum file-backed coverage per merged page.
    let mut merged_file_bytes: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for phdr in phdrs.iter() {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }
        let va_start = ((phdr.p_vaddr as usize) + load_bias) & !(PAGE_SIZE - 1);
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);
        let file_prefix = (phdr.p_offset as usize).saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + phdr.p_filesz as usize;
        let key = (va_start, file_offset_page_aligned);
        let cur = merged_file_bytes.get(&key).copied().unwrap_or(0);
        if file_backed_bytes > cur {
            merged_file_bytes.insert(key, file_backed_bytes);
        }
    }

    // 4. Create demand-paged VMAs for each PT_LOAD segment
    let mut brk_end: usize = 0;
    for phdr in phdrs {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }

        let va_start = ((phdr.p_vaddr as usize) + load_bias) & !(PAGE_SIZE - 1);
        let va_end_raw = phdr.p_vaddr as usize + load_bias + phdr.p_memsz as usize;
        let va_end = (va_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let prot = elf_flags_to_prot(phdr.p_flags);

        // file_offset is the page-aligned offset into the file
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);
        // Adjustment for non-page-aligned p_vaddr

        let obj_size = va_end - va_start;
        let file_prefix = (phdr.p_offset as usize).saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + phdr.p_filesz as usize;
        let merged_backed_bytes = merged_file_bytes
            .get(&(va_start, file_offset_page_aligned))
            .copied()
            .unwrap_or(file_backed_bytes);
        let obj = VmObject::new_anon(obj_size);
        obj.write().pager = Some(Arc::new(crate::mm::vm::VnodePager {
            vnode_id: vnode.vnode_id() as usize,
            path: alloc::string::String::from(vnode.path()),
            base_offset: file_offset_page_aligned,
            valid_bytes: merged_backed_bytes,
        }));

        let vma = VmMapEntry::new(
            va_start as u64,
            va_end as u64,
            BackingStore::Object {
                object: obj,
                offset: file_offset_page_aligned as u64,
            },
            EntryFlags::empty(),
            prot,
        );

        let mut vm = task.vm_map.lock();
        if let Err(e) = insert_or_merge_file_vma(&mut vm, vma) {
            return Err(map_insert_err(e));
        }

        if va_end > brk_end {
            brk_end = va_end;
        }
    }

    // Set initial brk to end of last PT_LOAD segment (page-aligned)
    task.brk
        .store(brk_end, core::sync::atomic::Ordering::Relaxed);

    // 4b. 如果有动态链接器 (PT_INTERP)，加载它并以其 entry 为入口
    let final_entry = if let Some(ref ipath) = interp_path {
        klog!(
            exec,
            debug,
            "exec pid={} loading interp={}",
            task.pid,
            ipath
        );
        match load_interp(task, ipath, DL_INTERP_OFFSET).await {
            Ok(interp_entry) => {
                klog!(
                    exec,
                    debug,
                    "exec pid={} interp entry={:#x}",
                    task.pid,
                    interp_entry
                );
                interp_entry
            },
            Err(e) => {
                klog!(
                    exec,
                    error,
                    "exec pid={} failed to load interp {}: {:?}",
                    task.pid,
                    ipath,
                    e
                );
                return Err(e);
            },
        }
    } else {
        entry
    };

    // 5. Create user stack VMA (anonymous, RW)
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let stack_obj = VmObject::new_anon(USER_STACK_SIZE);
    let stack_vma = VmMapEntry::new(
        stack_bottom as u64,
        USER_STACK_TOP as u64,
        BackingStore::Object {
            object: stack_obj,
            offset: 0,
        },
        EntryFlags::empty(),
        crate::map_perm!(R, W, U),
    );
    {
        let mut vm = task.vm_map.lock();
        if vm.insert_entry(stack_vma).is_err() {
            return Err(Errno::Enomem);
        }
    }

    // 6. Strip CLOEXEC fds (point of no return — exec will succeed)
    task.fd_table.lock().strip_cloexec();

    // 7. Map sigcode trampoline page for signal delivery
    {
        let vm_map = task.vm_map.lock();
        let mut pmap = vm_map.pmap_lock();
        map_sigcode_page(&mut pmap);
    }

    // 8. Reset signal dispositions: handlers reset to SIG_DFL on exec (POSIX)
    {
        let mut actions = task.signals.actions.lock();
        for act in actions.iter_mut() {
            if act.handler != SIG_DFL && act.handler != SIG_IGN {
                act.handler = SIG_DFL;
                act.flags = 0;
                act.mask = 0;
            }
        }
    }
    // Clear pending signals on exec
    task.signals
        .pending
        .store(SigSet(0), core::sync::atomic::Ordering::Relaxed);
    // Clear blocked signal mask on exec (POSIX: signal mask preserved, but
    // synchronous signals like SIGSEGV must be deliverable)
    task.signals
        .blocked
        .store(SigSet(0), core::sync::atomic::Ordering::Relaxed);

    klog!(
        exec,
        debug,
        "exec pid={} entry={:#x} sp={:#x}",
        task.pid,
        final_entry,
        USER_STACK_TOP
    );
    Ok((final_entry, USER_STACK_TOP))
}

// ---------------------------------------------------------------------------
// load_interp: legacy interpreter loader (operates on task's live vm_map)
// ---------------------------------------------------------------------------
#[cfg(feature = "exec-legacy")]
async fn load_interp(task: &Arc<Task>, interp_path: &str, offset: usize) -> Result<usize, Errno> {
    let vnode = resolve(interp_path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(Errno::Enoexec);
    }

    let obj = vnode_object(&*vnode);
    let page0 = VmObject::fetch_page_async(obj, crate::mm::vm::VObjIndex::new(0))
        .await
        .map_err(|_| Errno::Enoexec)?;

    let hdr_buf =
        unsafe { core::slice::from_raw_parts(page0.phys_addr.as_usize() as *const u8, PAGE_SIZE) };
    let elf_hdr = parse_elf_header(hdr_buf)?;
    let phdrs = parse_phdrs(hdr_buf, elf_hdr)?;
    let interp_entry = elf_hdr.e_entry as usize + offset;

    let mut merged_file_bytes: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for phdr in phdrs.iter() {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }
        let va_start = ((phdr.p_vaddr as usize) + offset) & !(PAGE_SIZE - 1);
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);
        let file_prefix = (phdr.p_offset as usize).saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + phdr.p_filesz as usize;
        let key = (va_start, file_offset_page_aligned);
        let cur = merged_file_bytes.get(&key).copied().unwrap_or(0);
        if file_backed_bytes > cur {
            merged_file_bytes.insert(key, file_backed_bytes);
        }
    }

    for phdr in phdrs {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }

        let va_start = ((phdr.p_vaddr as usize) + offset) & !(PAGE_SIZE - 1);
        let va_end_raw = phdr.p_vaddr as usize + offset + phdr.p_memsz as usize;
        let va_end = (va_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let prot = elf_flags_to_prot(phdr.p_flags);
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);

        let obj_size = va_end - va_start;
        let file_prefix = (phdr.p_offset as usize).saturating_sub(file_offset_page_aligned);
        let file_backed_bytes = file_prefix + phdr.p_filesz as usize;
        let merged_backed_bytes = merged_file_bytes
            .get(&(va_start, file_offset_page_aligned))
            .copied()
            .unwrap_or(file_backed_bytes);
        let obj = VmObject::new_anon(obj_size);
        obj.write().pager = Some(Arc::new(crate::mm::vm::VnodePager {
            vnode_id: vnode.vnode_id() as usize,
            path: alloc::string::String::from(vnode.path()),
            base_offset: file_offset_page_aligned,
            valid_bytes: merged_backed_bytes,
        }));
        let vma = VmMapEntry::new(
            va_start as u64,
            va_end as u64,
            BackingStore::Object {
                object: obj,
                offset: file_offset_page_aligned as u64,
            },
            EntryFlags::empty(),
            prot,
        );

        let mut vm = task.vm_map.lock();
        if let Err(e) = insert_or_merge_file_vma(&mut vm, vma) {
            return Err(map_insert_err(e));
        }
    }

    Ok(interp_entry)
}
