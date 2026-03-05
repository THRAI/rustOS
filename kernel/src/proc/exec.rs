//! ELF loading and exec() implementation.
//!
//! exec() parses ELF PT_LOAD headers, resets the vm_map, and creates
//! demand-paged VMAs with vnode + file_offset + file_size. No physical
//! frames are allocated at exec time — the fault handler does the rest.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal_common::{Errno, PhysAddr, VirtAddr, PAGE_SIZE};
use hal_common::addr::VirtPageNum;

use crate::fs::path;
use crate::fs::vnode::{VnodeType};
use crate::mm::vm::vm_map::{MapPerm, VmArea, VmAreaType, VmError, VmMap};
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
const ET_DYN: u16 = 3;
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// 动态链接器加载的固定偏移地址（参考 Chronix: 0x20_0000_0000）
const DL_INTERP_OFFSET: usize = 0x20_0000_0000;

/// User stack size: 64KB.
const USER_STACK_SIZE: usize = 64 * 1024;
/// Bridges `fs::vnode::Vnode` → `mm::vm::vm_map::Vnode` (two separate traits).
struct VnodeWrapper(Arc<dyn crate::fs::vnode::Vnode>);
impl crate::mm::vm::vm_map::Vnode for VnodeWrapper {
    fn vnode_id(&self) -> u64 { self.0.vnode_id() }
    fn path(&self) -> &str { self.0.path() }
}
/// User stack top address (just below kernel space).
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
    // Must be executable or shared object (PIE), RISC-V
    if (hdr.e_type != ET_EXEC && hdr.e_type != ET_DYN) || hdr.e_machine != EM_RISCV {
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

/// Insert a file-backed VMA, merging exact same-page overlaps produced by
/// ELF PT_LOAD headers that split permissions inside one page.
fn insert_or_merge_file_vma(vm: &mut VmMap, new_vma: VmArea) -> Result<(), VmError> {
    if new_vma.vma_type == VmAreaType::FileBacked {
        if let Some(existing) = vm.find_area_mut(new_vma.range.start) {
            let same_range = existing.range.start == new_vma.range.start
                && existing.range.end == new_vma.range.end;
            let same_file = match (existing.vnode.as_ref(), new_vma.vnode.as_ref()) {
                (Some(a), Some(b)) => a.vnode_id() == b.vnode_id(),
                _ => false,
            };
            let same_backing = existing.vma_type == VmAreaType::FileBacked
                && new_vma.vma_type == VmAreaType::FileBacked
                && existing.file_offset == new_vma.file_offset
                && existing.obj_offset == new_vma.obj_offset;

            if same_range && same_file && same_backing {
                // Merge segment perms (e.g. RX + R -> RX), and preserve the
                // longest file-visible byte span for partial last-page zeroing.
                existing.prot |= new_vma.prot;
                existing.file_size = core::cmp::max(existing.file_size, new_vma.file_size);
                return Ok(());
            }
        }
    }
    vm.insert(new_vma)
}

fn map_insert_err(err: VmError) -> Errno {
    match err {
        VmError::Overlap | VmError::InvalidRange => Errno::ENOEXEC,
        VmError::NotFound => Errno::ENOMEM,
    }
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

    // PIE: ET_DYN 需要一个加载基址，ET_EXEC 基址为 0
    let load_bias: usize = if elf_hdr.e_type == ET_DYN { 0x0 } else { 0x0 };
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

        let va_start = ((phdr.p_vaddr as usize) + load_bias) & !(PAGE_SIZE - 1);
        let va_end_raw = phdr.p_vaddr as usize + load_bias + phdr.p_memsz as usize;
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
            hal_common::addr::VirtPageNum(0),
            Arc::new(VnodeWrapper(Arc::clone(&vnode))),
            file_offset_page_aligned,
            file_size_in_vma as usize,
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
        klog!(exec, debug, "exec pid={} loading interp={}", task.pid, ipath);
        match load_interp(task, ipath, DL_INTERP_OFFSET).await {
            Ok(interp_entry) => {
                klog!(exec, debug, "exec pid={} interp entry={:#x}", task.pid, interp_entry);
                interp_entry
            }
            Err(e) => {
                klog!(exec, error, "exec pid={} failed to load interp {}: {:?}", task.pid, ipath, e);
                return Err(e);
            }
        }
    } else {
        entry
    };

    // 5. Create user stack VMA (anonymous, RW)
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let stack_obj = VmObject::new(USER_STACK_SIZE / PAGE_SIZE);
    let stack_vma = VmArea::new(
        VirtAddr::new(stack_bottom)..VirtAddr::new(USER_STACK_TOP),
        crate::map_perm!(R, W, U),
        stack_obj,
        hal_common::addr::VirtPageNum(0),
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
    task.signals.pending.store(super::signal::SigSet(0), core::sync::atomic::Ordering::Relaxed);
    // Clear blocked signal mask on exec (POSIX: signal mask preserved, but
    // synchronous signals like SIGSEGV must be deliverable)
    task.signals.blocked.store(super::signal::SigSet(0), core::sync::atomic::Ordering::Relaxed);

    klog!(exec, debug, "exec pid={} entry={:#x} sp={:#x}", task.pid, final_entry, USER_STACK_TOP);
    Ok((final_entry, USER_STACK_TOP))
}

// ---------------------------------------------------------------------------
// load_interp: 将动态链接器 ELF 加载到 offset 偏移处
// ---------------------------------------------------------------------------
async fn load_interp(task: &Arc<Task>, interp_path: &str, offset: usize) -> Result<usize, Errno> {
    let vnode = path::resolve(interp_path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(Errno::ENOEXEC);
    }

    let hdr_pa = match crate::fs::delegate::fs_read_page(vnode.path(), 0).await {
        Ok(pa) => {
            let pa = PhysAddr::new(pa);
            crate::fs::page_cache::complete(vnode.vnode_id(), 0, pa);
            pa
        }
        Err(_) => return Err(Errno::ENOEXEC),
    };

    let hdr_buf = unsafe {
        core::slice::from_raw_parts(hdr_pa.as_usize() as *const u8, PAGE_SIZE)
    };
    let elf_hdr = parse_elf_header(hdr_buf)?;
    let phdrs = parse_phdrs(hdr_buf, elf_hdr)?;
    let interp_entry = elf_hdr.e_entry as usize + offset;

    for phdr in phdrs {
        if phdr.p_type != PT_LOAD || phdr.p_memsz == 0 {
            continue;
        }

        let va_start = ((phdr.p_vaddr as usize) + offset) & !(PAGE_SIZE - 1);
        let va_end_raw = phdr.p_vaddr as usize + offset + phdr.p_memsz as usize;
        let va_end = (va_end_raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let prot = elf_flags_to_prot(phdr.p_flags);
        let file_offset_page_aligned = (phdr.p_offset as usize) & !(PAGE_SIZE - 1);
        let va_adjust = (phdr.p_vaddr as usize + offset) - va_start;
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
            hal_common::addr::VirtPageNum(0),
            Arc::new(VnodeWrapper(Arc::clone(&vnode))),
            file_offset_page_aligned,
            file_size_in_vma as usize,
        );

        let mut vm = task.vm_map.lock();
        if let Err(e) = insert_or_merge_file_vma(&mut vm, vma) {
            return Err(map_insert_err(e));
        }
    }

    Ok(interp_entry)
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
        let page_idx = VirtPageNum(
            (stack_page_va - vma.range.start.as_usize()) / PAGE_SIZE
                + vma.obj_offset.as_usize(),
        );
        let mut obj = vma.object.write();
        let typed_frame = crate::mm::allocator::TypedFrame {
            phys: frame,
            _marker: core::marker::PhantomData::<crate::mm::allocator::UserAnon>,
        };
        obj.insert_page(
            page_idx,
            crate::mm::vm::vm_object::OwnedPage::new_anonymous(typed_frame),
        );
    }

    // Map in pmap
    {
        let mut pmap = task.pmap.lock();
        let _ = crate::mm::pmap::pmap_enter(
            &mut pmap,
            VirtAddr::new(stack_page_va),
            frame,
            crate::map_perm!(R, W, U),
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
    let slice_ptr: *mut u8;
    {
        let allocated = cursor.alloc_down_bytes(slots_bytes).ok_or(Errno::ENOMEM)?;
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
