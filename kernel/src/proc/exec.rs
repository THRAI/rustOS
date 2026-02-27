//! ELF loading and exec() implementation.
//!
//! exec() parses ELF PT_LOAD headers, resets the vm_map, and creates
//! demand-paged VMAs with vnode + file_offset + file_size. No physical
//! frames are allocated at exec time — the fault handler does the rest.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal_common::{Errno, PhysAddr, VirtAddr, PAGE_SIZE};

use crate::fs::path;
use crate::fs::vnode::VnodeType;
use crate::mm::vm::vm_map::{MapPerm, VmArea, VmAreaType};
use crate::mm::vm::vm_object::VmObject;

use super::task::Task;

// ---------------------------------------------------------------------------
// ELF64 structures (hand-rolled, no external crate)
// ---------------------------------------------------------------------------

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_RISCV: u16 = 243;
const ET_EXEC: u16 = 2;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// User stack size: 64KB.
const USER_STACK_SIZE: usize = 64 * 1024;
/// Helper wrapper to map `fs::vnode::Vnode` into `mm::vm::vm_map::Vnode`.
struct VnodeWrapper(Arc<dyn crate::fs::vnode::Vnode>);

impl crate::mm::vm::vm_map::Vnode for VnodeWrapper {
    fn vnode_id(&self) -> u64 {
        self.0.vnode_id()
    }
    fn path(&self) -> &str {
        self.0.path()
    }
}

/// Helper struct for `sys_execve_async`.s (just below kernel space).
const USER_STACK_TOP: usize = 0x0000_003F_FFFF_F000;

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

// ---------------------------------------------------------------------------
// ELF parsing helpers
// ---------------------------------------------------------------------------

/// Parse ELF64 header from a byte buffer. Returns None if invalid.
fn parse_elf_header(buf: &[u8]) -> Result<&Elf64Header, Errno> {
    if buf.len() < core::mem::size_of::<Elf64Header>() {
        return Err(Errno::ENOEXEC);
    }
    let hdr = unsafe { &*(buf.as_ptr() as *const Elf64Header) };

    // Validate magic
    if hdr.e_ident[0..4] != ELF_MAGIC {
        return Err(Errno::ENOEXEC);
    }
    // Must be 64-bit, little-endian
    if hdr.e_ident[4] != ELFCLASS64 || hdr.e_ident[5] != ELFDATA2LSB {
        return Err(Errno::ENOEXEC);
    }
    // Must be executable, RISC-V
    if hdr.e_type != ET_EXEC || hdr.e_machine != EM_RISCV {
        return Err(Errno::ENOEXEC);
    }
    Ok(hdr)
}

/// Parse program headers from buffer.
fn parse_phdrs<'a>(buf: &'a [u8], hdr: &Elf64Header) -> Result<&'a [Elf64Phdr], Errno> {
    let off = hdr.e_phoff as usize;
    let num = hdr.e_phnum as usize;
    let ent = hdr.e_phentsize as usize;
    let end = off + num * ent;
    if end > buf.len() || ent < core::mem::size_of::<Elf64Phdr>() {
        return Err(Errno::ENOEXEC);
    }
    let ptr = unsafe { buf.as_ptr().add(off) as *const Elf64Phdr };
    Ok(unsafe { core::slice::from_raw_parts(ptr, num) })
}

/// Convert ELF p_flags to MapPerm.
fn elf_flags_to_prot(flags: u32) -> MapPerm {
    let mut prot = MapPerm::U; // Always user-accessible
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

// ---------------------------------------------------------------------------
// exec()
// ---------------------------------------------------------------------------

/// Execute an ELF binary: resolve path, parse ELF, reset vm_map, create
/// demand-paged VMAs, set up user stack. Returns (entry_point, stack_pointer).
///
/// This is an async function because path resolution and ELF header reading
/// go through the delegate channel.
pub async fn exec(task: &Arc<Task>, elf_path: &str) -> Result<(usize, usize), Errno> {
    klog!(exec, debug, "exec pid={} path={}", task.pid, elf_path);
    // 1. Resolve path to vnode
    let vnode = path::resolve(elf_path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(Errno::ENOEXEC);
    }

    // 2. Read ELF header + program headers (first 4KB is enough)
    let file_size = vnode.size();
    if file_size < core::mem::size_of::<Elf64Header>() as u64 {
        return Err(Errno::ENOEXEC);
    }

    // Read first page via delegate to get headers
    let hdr_pa = match crate::fs::delegate::fs_read_page(vnode.path(), 0).await {
        Ok(pa) => {
            let pa = PhysAddr::new(pa);
            crate::fs::page_cache::complete(vnode.vnode_id(), 0, pa);
            pa
        }
        Err(_) => return Err(Errno::ENOEXEC),
    };

    // Parse from the physical page (identity-mapped in kernel)
    let hdr_buf = unsafe { core::slice::from_raw_parts(hdr_pa.as_usize() as *const u8, PAGE_SIZE) };
    let elf_hdr = parse_elf_header(hdr_buf)?;
    let phdrs = parse_phdrs(hdr_buf, elf_hdr)?;
    let entry = elf_hdr.e_entry as usize;

    // 3. Reset vm_map and pmap: tear down old address space
    {
        let mut vm = task.vm_map.lock();
        vm.clear();
    }
    {
        let mut pmap = task.pmap.lock();
        // Create the new pmap BEFORE destroying the old one.  We must switch
        // satp to the new root page table before freeing the old root,
        // otherwise any TLB miss would walk freed memory (use-after-free on
        // the page table), causing a cascading fault.
        let mut old_pmap = core::mem::replace(&mut *pmap, crate::mm::pmap::pmap_create());
        crate::mm::pmap::pmap_activate(&mut pmap);
        // Now satp points to the new root — safe to free the old page tables.
        crate::mm::pmap::pmap_destroy(&mut old_pmap);
    }

    // 4. Create demand-paged VMAs for each PT_LOAD segment
    let mut brk_end: usize = 0;
    for phdr in phdrs {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }

        let va_start = (phdr.p_vaddr as usize) & !(PAGE_SIZE - 1);
        let va_end_raw = phdr.p_vaddr as usize + phdr.p_memsz as usize;
        let va_end = (va_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let prot = elf_flags_to_prot(phdr.p_flags);

        // file_offset is the page-aligned offset into the file
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);
        // Adjustment for non-page-aligned p_vaddr
        let va_adjust = phdr.p_vaddr as usize - va_start;
        let file_size_in_vma = if phdr.p_filesz > 0 {
            phdr.p_filesz as u64 + va_adjust as u64
        } else {
            0
        };

        let obj_size = (va_end - va_start) / PAGE_SIZE;
        let obj = VmObject::new(obj_size);

        let vma = VmArea::new_file_backed(
            VirtAddr::new(va_start)..VirtAddr::new(va_end),
            prot,
            obj,
            0,
            Arc::new(VnodeWrapper(Arc::clone(&vnode))),
            file_offset_page_aligned as u64,
            file_size_in_vma,
        );

        let mut vm = task.vm_map.lock();
        if vm.insert(vma).is_err() {
            return Err(Errno::ENOMEM);
        }

        if va_end > brk_end {
            brk_end = va_end;
        }
    }

    // Set initial brk to end of last PT_LOAD segment (page-aligned)
    task.brk
        .store(brk_end, core::sync::atomic::Ordering::Relaxed);

    // 5. Create user stack VMA (anonymous, RW)
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let stack_obj = VmObject::new(USER_STACK_SIZE / PAGE_SIZE);
    let stack_vma = VmArea::new(
        VirtAddr::new(stack_bottom)..VirtAddr::new(USER_STACK_TOP),
        MapPerm::R | MapPerm::W | MapPerm::U,
        stack_obj,
        0,
        VmAreaType::Stack,
    );
    {
        let mut vm = task.vm_map.lock();
        if vm.insert(stack_vma).is_err() {
            return Err(Errno::ENOMEM);
        }
    }

    // 6. Strip CLOEXEC fds (point of no return — exec will succeed)
    task.fd_table.lock().strip_cloexec();

    // 7. Map sigcode trampoline page for signal delivery
    {
        let mut pmap = task.pmap.lock();
        super::signal::map_sigcode_page(&mut pmap);
    }

    // 8. Reset signal dispositions: handlers reset to SIG_DFL on exec (POSIX)
    {
        let mut actions = task.signals.actions.lock();
        for act in actions.iter_mut() {
            if act.handler != super::signal::SIG_DFL && act.handler != super::signal::SIG_IGN {
                act.handler = super::signal::SIG_DFL;
                act.flags = 0;
                act.mask = 0;
            }
        }
    }
    // Clear pending signals on exec
    task.signals
        .pending
        .store(0, core::sync::atomic::Ordering::Relaxed);
    // Note: POSIX requires blocked signal mask to be preserved across exec.
    // We intentionally DO NOT clear task.signals.blocked here.

    klog!(
        exec,
        debug,
        "exec pid={} entry={:#x} sp={:#x}",
        task.pid,
        entry,
        USER_STACK_TOP
    );
    Ok((entry, USER_STACK_TOP))
}

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
// exec_with_args: exec + argc/argv/envp/auxv stack setup
// ---------------------------------------------------------------------------

/// Execute an ELF binary with argv/envp, building the initial user stack
/// layout that musl/glibc expects.
///
/// Stack layout (grows downward, sp points to argc):
/// ```text
///   strings area (argv[i] data, envp[i] data, random bytes)
///   padding to 16-byte alignment
///   auxv[N] = {AT_NULL, 0}
///   auxv[...] = {type, value}
///   NULL (envp terminator)
///   envp[...] pointers
///   NULL (argv terminator)
///   argv[...] pointers
///   argc          <-- sp
/// ```
pub async fn exec_with_args(
    task: &Arc<Task>,
    elf_path: &str,
    argv: &[String],
    envp: &[String],
) -> Result<(usize, usize), Errno> {
    // Run the base exec (ELF load, VMA setup, stack VMA creation)
    let (entry, _) = exec(task, elf_path).await?;

    // Eagerly allocate and map the top stack page so we can write to it.
    let stack_page_va = USER_STACK_TOP - PAGE_SIZE;
    let frame = crate::mm::allocator::frame_alloc_sync().ok_or(Errno::ENOMEM)?;
    crate::mm::pmap::pmap_zero_page(frame);

    // Insert page into the stack VMA's VmObject
    {
        let vm = task.vm_map.lock();
        let vma = vm
            .find_area(VirtAddr::new(stack_page_va))
            .ok_or(Errno::ENOMEM)?;
        let page_idx =
            ((stack_page_va - vma.range.start.as_usize()) / PAGE_SIZE) as u64 + vma.obj_offset;
        let mut obj = vma.object.write();
        obj.insert_page(
            page_idx,
            crate::mm::vm::vm_object::OwnedPage::new_anonymous(frame), //TODO: reimport
        );
    }

    // Map in pmap
    {
        let mut pmap = task.pmap.lock();
        let _ = crate::mm::pmap::pmap_enter(
            &mut pmap,
            VirtAddr::new(stack_page_va),
            frame,
            MapPerm::R | MapPerm::W | MapPerm::U,
            false,
        );
    }

    // Now write the stack layout into the physical frame (identity-mapped).
    let mut cursor = hal_common::addr::PageCursor::new(frame, PAGE_SIZE).unwrap();
    let stack_page_vbase = VirtAddr::new(stack_page_va);

    // Push 16 random bytes for AT_RANDOM
    let random_va = {
        let slice = cursor.alloc_down_bytes(16).unwrap();
        slice.fill(0); // Zero is fine for now (deterministic but functional)
        cursor.current_va(stack_page_vbase)
    };

    // Push all strings and record their VAs
    let mut argv_vas: Vec<usize> = Vec::with_capacity(argv.len());
    for arg in argv {
        let s = arg.as_bytes();
        let len = s.len() + 1; // include NUL
        let slice = cursor.alloc_down_bytes(len).unwrap();
        slice[..s.len()].copy_from_slice(s);
        slice[s.len()] = 0; // NUL terminator
        argv_vas.push(cursor.current_va(stack_page_vbase).as_usize());
    }
    let mut envp_vas: Vec<usize> = Vec::with_capacity(envp.len());
    for env in envp {
        let s = env.as_bytes();
        let len = s.len() + 1; // include NUL
        let slice = cursor.alloc_down_bytes(len).unwrap();
        slice[..s.len()].copy_from_slice(s);
        slice[s.len()] = 0; // NUL terminator
        envp_vas.push(cursor.current_va(stack_page_vbase).as_usize());
    }

    // Build auxv entries
    let auxv: [(usize, usize); 4] = [
        (AT_PAGESZ, PAGE_SIZE),
        (AT_ENTRY, entry),
        (AT_RANDOM, random_va.as_usize()),
        (AT_NULL, 0),
    ];

    // Calculate how many usize slots we need below the strings:
    // argc + argv ptrs + NULL + envp ptrs + NULL + auxv pairs
    let n_slots = 1 + argv.len() + 1 + envp.len() + 1 + auxv.len() * 2;
    let slots_bytes = n_slots * core::mem::size_of::<usize>();

    // Align down to 16 bytes for ABI, subtract slots, and align again.
    cursor.align_down(16);

    // First retrieve the current SP and compute the offset
    let prev_offset = cursor.current_offset();
    if prev_offset < slots_bytes {
        return Err(Errno::ENOMEM);
    }
    let new_offset = prev_offset - slots_bytes;
    let sp_va = stack_page_vbase.as_usize() + new_offset;

    // Now we safely do the allocation to get the mutable slice
    let mut slice_ptr: *mut u8;
    {
        let mut allocated = cursor.alloc_down_bytes(slots_bytes).ok_or(Errno::ENOMEM)?;
        slice_ptr = allocated.as_mut_ptr();
    }
    // Safety: we just created this slice and we know it lives inside our `frame`.
    let slice = unsafe { core::slice::from_raw_parts_mut(slice_ptr, slots_bytes) };

    // Write the slots using our pre-allocated slice
    let mut offset = 0;
    let mut write_usize = |val: usize| {
        slice[offset..offset + core::mem::size_of::<usize>()].copy_from_slice(&val.to_ne_bytes());
        offset += core::mem::size_of::<usize>();
    };

    // argc
    write_usize(argv.len());
    // argv pointers
    for va in &argv_vas {
        write_usize(*va);
    }
    write_usize(0); // NULL terminator

    // envp pointers
    for va in &envp_vas {
        write_usize(*va);
    }
    write_usize(0); // NULL terminator

    // auxv
    for &(atype, aval) in &auxv {
        write_usize(atype);
        write_usize(aval);
    }

    Ok((entry, sp_va))
}
