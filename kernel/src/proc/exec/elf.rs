//! Pure ELF parsing — no kernel types, no VM, no IO.
//!
//! This module takes raw bytes and produces a platform-agnostic parse result
//! (`ElfParseResult`) that downstream consumers can interpret however they
//! like.  It deliberately avoids `VirtAddr`, `MapPerm`, `VmMap`, or any
//! other kernel-specific abstraction so that:
//!
//!   1. It can be tested in user-space with `cargo test` (no kernel stubs).
//!   2. The parse artifact is an **immutable input** to the exec pipeline,
//!      never the mutable accumulator.
//!
//! # Errors
//!
//! All fallible paths return `ElfError`, a self-contained enum that does
//! not depend on `Errno`.  The boundary code (`exec.rs`) maps `ElfError`
//! to `Errno` at the IO shell level.

use alloc::{string::String, vec::Vec};

use goblin::elf::{
    header::{EM_LOONGARCH, EM_RISCV, ET_DYN, ET_EXEC},
    program_header::{PT_INTERP, PT_LOAD, PT_PHDR},
    Header, ProgramHeader,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Every distinct way ELF parsing can fail.
///
/// Deliberately does not carry `Errno` — the IO shell maps these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElfError {
    /// goblin returned a parse error (magic, class, endianness, etc.).
    ParseFailed(String),
    /// e_type is not ET_EXEC or ET_DYN.
    UnsupportedType(u16),
    /// e_machine does not match the current kernel architecture.
    UnsupportedMachine(u16),
    /// Program headers extend beyond the provided buffer.
    PhdrOverflow { phdr_end: usize, buffer_len: usize },
}

// ---------------------------------------------------------------------------
// Parse result — immutable, platform-agnostic
// ---------------------------------------------------------------------------

/// A single PT_LOAD segment, stored with raw ELF field values.
///
/// No load-base adjustment, no MapPerm conversion — those happen at the
/// pipeline boundary in `exec_ctx.rs`.
#[derive(Debug, Clone)]
pub struct ElfSegment {
    /// `p_vaddr` — the segment's preferred virtual address (un-relocated).
    pub vaddr: u64,
    /// `p_memsz` — total size in memory (including BSS tail).
    pub memsz: u64,
    /// `p_filesz` — file-backed portion.
    pub filesz: u64,
    /// `p_offset` — offset into the ELF file.
    pub offset: u64,
    /// Raw `p_flags` (PF_R=4, PF_W=2, PF_X=1).
    pub flags: u32,
}

/// The complete, immutable result of parsing an ELF binary.
///
/// Consumers should treat this as read-only input.  All kernel-specific
/// interpretation (load-base relocation, flag conversion, brk computation)
/// happens downstream in the pipeline.
#[derive(Debug, Clone)]
pub struct ElfParseResult {
    /// `e_entry` — raw entry point from the ELF header.
    pub entry: u64,
    /// All PT_LOAD segments in program-header order.
    pub segments: Vec<ElfSegment>,
    /// PT_INTERP path, if present (dynamic linker).
    pub interp: Option<String>,
    /// PT_PHDR virtual address (for auxv AT_PHDR).
    pub phdr_vaddr: u64,
    /// `e_phnum`.
    pub phnum: u16,
    /// `e_phentsize`.
    pub phentsize: u16,
    /// `true` when `e_type == ET_DYN` (PIE / shared object).
    pub is_dyn: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[cfg(target_arch = "riscv64")]
const EXPECTED_E_MACHINE: u16 = EM_RISCV;

#[cfg(target_arch = "loongarch64")]
const EXPECTED_E_MACHINE: u16 = EM_LOONGARCH;

/// Validate that a parsed ELF header targets the running kernel architecture.
///
/// Checks `e_type` (ET_EXEC or ET_DYN) and `e_machine`.
/// goblin's own `parse_header` already validated magic, class, and
/// endianness, so we only check the fields it leaves open.
pub fn validate_header(ehdr: &Header) -> Result<(), ElfError> {
    if ehdr.e_type != ET_EXEC && ehdr.e_type != ET_DYN {
        return Err(ElfError::UnsupportedType(ehdr.e_type));
    }
    if ehdr.e_machine != EXPECTED_E_MACHINE {
        return Err(ElfError::UnsupportedMachine(ehdr.e_machine));
    }
    Ok(())
}

/// Parse an ELF header and all program headers from raw bytes.
///
/// `bytes` is typically the first page of the file (4096 bytes).
/// Returns the raw goblin header and program-header vector on success.
///
/// # Panics
///
/// None — returns `ElfError::PhdrOverflow` instead of panicking when
/// program headers extend beyond the buffer.
fn parse_header_and_phdrs(bytes: &[u8]) -> Result<(Header, Vec<ProgramHeader>), ElfError> {
    let header = goblin::elf::Elf::parse_header(bytes)
        .map_err(|e| ElfError::ParseFailed(alloc::format!("{}", e)))?;

    let phoff = header.e_phoff as usize;
    let phnum = header.e_phnum as usize;
    let phentsize = header.e_phentsize as usize;
    let phdr_end = phoff + phnum * phentsize;

    if phdr_end > bytes.len() {
        return Err(ElfError::PhdrOverflow {
            phdr_end,
            buffer_len: bytes.len(),
        });
    }

    let container = header
        .container()
        .map_err(|e| ElfError::ParseFailed(alloc::format!("{}", e)))?;
    let endianness = header
        .endianness()
        .map_err(|e| ElfError::ParseFailed(alloc::format!("{}", e)))?;
    let ctx = goblin::container::Ctx::new(container, endianness);

    let phdrs = ProgramHeader::parse(bytes, phoff, phnum, ctx)
        .map_err(|e| ElfError::ParseFailed(alloc::format!("{}", e)))?;

    Ok((header, phdrs))
}

/// Parse a complete `ElfParseResult` from the first page of an ELF file.
///
/// This is the single entry point for all ELF consumption.  It validates
/// the header, extracts every PT_LOAD segment, the PT_INTERP path (if
/// any), and the PT_PHDR address.
///
/// **Pure function**: no IO, no kernel types, no side effects.
pub fn parse_elf(bytes: &[u8]) -> Result<ElfParseResult, ElfError> {
    let (ehdr, phdrs) = parse_header_and_phdrs(bytes)?;
    validate_header(&ehdr)?;

    let mut segments = Vec::new();
    let mut interp: Option<String> = None;
    let mut phdr_vaddr: u64 = 0;

    for ph in &phdrs {
        match ph.p_type {
            PT_LOAD => {
                segments.push(ElfSegment {
                    vaddr: ph.p_vaddr,
                    memsz: ph.p_memsz,
                    filesz: ph.p_filesz,
                    offset: ph.p_offset,
                    flags: ph.p_flags,
                });
            },
            PT_INTERP => {
                let start = ph.p_offset as usize;
                let end = (ph.p_offset + ph.p_filesz) as usize;
                if end <= bytes.len() {
                    let raw = &bytes[start..end];
                    let trimmed = raw.split(|&b| b == 0).next().unwrap_or(raw);
                    if let Ok(s) = core::str::from_utf8(trimmed) {
                        interp = Some(String::from(s));
                    }
                }
            },
            PT_PHDR => {
                phdr_vaddr = ph.p_vaddr;
            },
            _ => {}, // PT_NOTE, PT_GNU_STACK, etc.
        }
    }

    Ok(ElfParseResult {
        entry: ehdr.e_entry,
        segments,
        interp,
        phdr_vaddr,
        phnum: ehdr.e_phnum,
        phentsize: ehdr.e_phentsize,
        is_dyn: ehdr.e_type == ET_DYN,
    })
}
