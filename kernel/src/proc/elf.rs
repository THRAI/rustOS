use crate::hal_common::VirtAddr;
use crate::mm::vm::MapPerm;
use alloc::string::String;
use alloc::vec::Vec;

/// A single loadable memory region parsed from an ELF PT_LOAD segment.
#[derive(Debug, Clone)]
pub struct MemoryRegion {
    pub vaddr: usize,
    pub memsz: usize,
    pub filesz: usize,
    pub offset: usize,
    pub prot: MapPerm,
}

/// Execution context built from parsed ELF headers.
///
/// This is a pure data structure: `build()` does no I/O, no allocation
/// beyond Vec, and touches no VmMap.  The caller uses the region list
/// to construct VmMapEntries separately.
#[derive(Debug)]
pub struct ExecContext {
    pub load_base: VirtAddr,
    pub mapped_regions: Vec<MemoryRegion>,
    pub brk: VirtAddr,

    pub user_entry: VirtAddr,
    pub interp_entry: Option<VirtAddr>,

    pub interp_path: Option<String>,

    pub phdr_vaddr: VirtAddr,
    pub phnum: u16,
    pub phentsize: u16,
}

impl Default for ExecContext {
    fn default() -> Self {
        Self {
            load_base: VirtAddr(0),
            mapped_regions: Vec::new(),
            brk: VirtAddr(0),
            user_entry: VirtAddr(0),
            interp_entry: None,
            interp_path: None,
            phdr_vaddr: VirtAddr(0),
            phnum: 0,
            phentsize: 0,
        }
    }
}

impl ExecContext {
    /// Build an ExecContext from parsed ELF headers.
    ///
    /// Pure function: takes the raw first-page bytes (for PT_INTERP string
    /// extraction), the goblin header, program headers, and a load base
    /// address.  Returns a fully populated ExecContext with all PT_LOAD
    /// regions, interpreter path, and brk.
    pub fn build(
        elf_bytes: &[u8],
        ehdr: &goblin::elf::Header,
        phdrs: &[goblin::elf::ProgramHeader],
        load_base: VirtAddr,
    ) -> Self {
        let mut ctx = Self {
            load_base,
            user_entry: VirtAddr(ehdr.e_entry as usize + load_base.0),
            phnum: ehdr.e_phnum,
            phentsize: ehdr.e_phentsize,
            ..Default::default()
        };

        let mut max_vaddr: usize = 0;

        for phdr in phdrs {
            match phdr.p_type {
                // PT_LOAD: build memory region descriptors
                goblin::elf::program_header::PT_LOAD => {
                    let region = MemoryRegion {
                        vaddr: phdr.p_vaddr as usize + load_base.0,
                        memsz: phdr.p_memsz as usize,
                        filesz: phdr.p_filesz as usize,
                        offset: phdr.p_offset as usize,
                        prot: elf_flags_to_mapperm(phdr.p_flags),
                    };
                    ctx.mapped_regions.push(region);

                    // Track program break (highest segment end)
                    let segment_end = phdr.p_vaddr as usize + load_base.0 + phdr.p_memsz as usize;
                    if segment_end > max_vaddr {
                        max_vaddr = segment_end;
                    }
                }

                // PT_INTERP: extract dynamic linker path
                goblin::elf::program_header::PT_INTERP => {
                    let start = phdr.p_offset as usize;
                    let end = (phdr.p_offset + phdr.p_filesz) as usize;
                    if end <= elf_bytes.len() {
                        // Strip trailing NUL
                        let raw = &elf_bytes[start..end];
                        let trimmed = raw.split(|&b| b == 0).next().unwrap_or(raw);
                        if let Ok(interp_str) = core::str::from_utf8(trimmed) {
                            ctx.interp_path = Some(String::from(interp_str));
                        }
                    }
                }

                // PT_PHDR: record program header table VA for auxv AT_PHDR
                goblin::elf::program_header::PT_PHDR => {
                    ctx.phdr_vaddr = VirtAddr(phdr.p_vaddr as usize + load_base.0);
                }
                _ => {} // Ignore PT_NOTE, PT_GNU_STACK, etc.
            }
        }

        // Align brk up to page boundary
        let page_size: usize = 4096;
        ctx.brk = VirtAddr((max_vaddr + page_size - 1) & !(page_size - 1));

        ctx
    }
}

/// Convert ELF p_flags (PF_R=4, PF_W=2, PF_X=1) to MapPerm bitflags.
///
/// Always includes MapPerm::U (user-accessible) since these are
/// user-space ELF segments.
pub fn elf_flags_to_mapperm(flags: u32) -> MapPerm {
    const PF_X: u32 = 1;
    const PF_W: u32 = 2;
    const PF_R: u32 = 4;

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

impl From<u32> for MapPerm {
    /// Convert ELF p_flags to MapPerm (convenience for `.into()` calls).
    fn from(flags: u32) -> Self {
        elf_flags_to_mapperm(flags)
    }
}

use goblin::elf::{Header, ProgramHeader};
use goblin::error::Error;

/// Parse ELF header and program headers from the first page of an ELF file.
///
/// The first page (typically 4096 bytes) must contain the full ELF header
/// and all program headers.  Panics if program headers extend beyond the
/// provided buffer.
pub fn parse_elf_first_page(page_bytes: &[u8]) -> Result<(Header, Vec<ProgramHeader>), Error> {
    // Parse unified Header via Elf::parse_header (uses scroll::Pread internally)
    let header = goblin::elf::Elf::parse_header(page_bytes)?;

    let phoff = header.e_phoff as usize;
    let phnum = header.e_phnum as usize;
    let phentsize = header.e_phentsize as usize;
    let phdrs_total_size = phnum * phentsize;

    if phoff + phdrs_total_size > page_bytes.len() {
        panic!("Program Header larger than 1 page")
    }

    // Build Ctx from header for ProgramHeader::parse
    let container = header.container()?;
    let endianness = header.endianness()?;
    let ctx = goblin::container::Ctx::new(container, endianness);

    let phdrs = ProgramHeader::parse(page_bytes, phoff, phnum, ctx)?;

    Ok((header, phdrs))
}
