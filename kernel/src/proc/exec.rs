//! ELF loading and exec() implementation.
//!
//! exec() parses ELF PT_LOAD headers, resets the vm_map, and creates
//! demand-paged VMAs with vnode + file_offset + file_size. No physical
//! frames are allocated at exec time — the fault handler does the rest.

use alloc::sync::Arc;
use hal_common::{Errno, PhysAddr, VirtAddr, PAGE_SIZE};
use spin::RwLock;

use crate::fs::path;
use crate::fs::vnode::{Vnode, VnodeType};
use crate::mm::vm::vm_map::{MapPerm, VmArea, VmAreaType, VmMap};
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
    if flags & PF_R != 0 { prot |= MapPerm::R; }
    if flags & PF_W != 0 { prot |= MapPerm::W; }
    if flags & PF_X != 0 { prot |= MapPerm::X; }
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
    let ino = vnode.vnode_id() as u32;
    let hdr_pa = match crate::fs::delegate::fs_read_page(ino, 0).await {
        Ok(pa) => {
            let pa = PhysAddr::new(pa);
            crate::fs::page_cache::complete(vnode.vnode_id(), 0, pa);
            pa
        }
        Err(_) => return Err(Errno::ENOEXEC),
    };

    // Parse from the physical page (identity-mapped in kernel)
    let hdr_buf = unsafe {
        core::slice::from_raw_parts(hdr_pa.as_usize() as *const u8, PAGE_SIZE)
    };
    let elf_hdr = parse_elf_header(hdr_buf)?;
    let phdrs = parse_phdrs(hdr_buf, elf_hdr)?;
    let entry = elf_hdr.e_entry as usize;

    // 3. Reset vm_map: tear down old address space
    {
        let mut vm = task.vm_map.lock();
        vm.clear();
    }

    // 4. Create demand-paged VMAs for each PT_LOAD segment
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
            Arc::clone(&vnode) as Arc<dyn Vnode>,
            file_offset_page_aligned as u64,
            file_size_in_vma,
        );

        let mut vm = task.vm_map.lock();
        if vm.insert(vma).is_err() {
            return Err(Errno::ENOMEM);
        }
    }

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

    // 6. Reset fd table: keep stdin/stdout/stderr (0,1,2), close rest
    // For now, just leave fd table as-is (exec preserves fds per POSIX)

    Ok((entry, USER_STACK_TOP))
}
