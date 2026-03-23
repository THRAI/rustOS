//! Monadic exec pipeline — pure transforms over a single accumulator.
//!
//! `ExecPipeline` is the state threaded through every stage-2 step.
//! Every `with_*` method has the signature:
//!
//!     fn with_*(self, ...) -> Result<Self, Errno>
//!
//! This is the Rust equivalent of the Haskell pattern:
//!
//!     type ExecM a = ExecPipeline -> Either Errno (a, ExecPipeline)
//!
//! except that when the "output" is always `()` we collapse it to just
//! `ExecPipeline -> Result<ExecPipeline, Errno>`, i.e. `State Errno ()`.
//!
//! The pipeline carries **all** accumulated state inside one struct:
//! the `VmMap`, the resolved entry point, the stack object handle, auxv
//! entries, and brk.  No step returns a tuple — the caller never has to
//! destructure intermediate results or change the chain type.
//!
//! # Purity contract
//!
//! Nothing in this module performs IO (no pager fetches, no pmap writes,
//! no vnode resolution).  All IO lives in `exec.rs`.  The only allocations
//! are `Vec::push` and `VmObject` descriptor construction (lazy — pages
//! are not faulted in until first access).

use alloc::{collections::BTreeMap, string::String, sync::Arc, vec, vec::Vec};

use crate::{
    hal_common::{Errno, SpinMutex, VirtAddr, PAGE_SIZE},
    mm::{
        vm::{MapPerm, VmMapping},
        VmMap, VmObject,
    },
    proc::exec::elf::{ElfParseResult, ElfSegment},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// User stack size: 64 KiB.
const USER_STACK_SIZE: usize = 64 * 1024;
/// User stack top address (just below kernel space).
const USER_STACK_TOP: usize = 0x0000_003F_FFFF_F000;

// ---------------------------------------------------------------------------
// Flag conversion (boundary between ELF and kernel)
// ---------------------------------------------------------------------------

/// Convert raw ELF `p_flags` (PF_R=4, PF_W=2, PF_X=1) to `MapPerm`.
///
/// Always includes `MapPerm::U` since these are user-space segments.
/// This is the *only* place where raw ELF flags enter kernel-land.
#[inline]
pub fn elf_flags_to_perm(flags: u32) -> MapPerm {
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
    fn from(flags: u32) -> Self {
        elf_flags_to_perm(flags)
    }
}

// ---------------------------------------------------------------------------
// MemoryRegion — internal, derived from ElfSegment + load_base
// ---------------------------------------------------------------------------

/// A segment relocated to its final virtual address.
///
/// Created inside `ExecPipeline::with_segments` — not part of the
/// public API.  The caller never constructs these directly.
#[derive(Debug, Clone)]
struct MemoryRegion {
    vaddr: usize,
    memsz: usize,
    filesz: usize,
    offset: usize,
    prot: MapPerm,
}

impl MemoryRegion {
    /// Build from a raw `ElfSegment` plus a load-base offset.
    fn from_segment(seg: &ElfSegment, load_base: usize) -> Self {
        Self {
            vaddr: seg.vaddr as usize + load_base,
            memsz: seg.memsz as usize,
            filesz: seg.filesz as usize,
            offset: seg.offset as usize,
            prot: elf_flags_to_perm(seg.flags),
        }
    }

    #[inline]
    fn va_start(&self) -> VirtAddr {
        VirtAddr::new_page_aligned_down(self.vaddr)
    }

    #[inline]
    fn va_end(&self) -> VirtAddr {
        VirtAddr::new_page_aligned_up(self.vaddr + self.memsz)
    }

    #[inline]
    fn page_count(&self) -> usize {
        (self.va_end().as_usize() - self.va_start().as_usize()) / PAGE_SIZE
    }

    #[inline]
    fn file_offset_aligned(&self) -> usize {
        self.offset & !(PAGE_SIZE - 1)
    }

    #[inline]
    fn file_prefix(&self) -> usize {
        self.offset & (PAGE_SIZE - 1)
    }

    #[inline]
    fn file_backed_bytes(&self) -> usize {
        self.file_prefix() + self.filesz
    }

    #[inline]
    fn merge_key(&self) -> (usize, usize) {
        (self.va_start().as_usize(), self.file_offset_aligned())
    }
}

/// A single page-aligned file-backed mapping derived from one or more ELF
/// segments that overlap the same final page.
#[derive(Debug, Clone, Copy)]
struct PageRegion {
    page_va: usize,
    file_offset: usize,
    file_backed: usize,
    prot: MapPerm,
}

fn normalize_page_regions(parse: &ElfParseResult, load_base: usize) -> Vec<PageRegion> {
    let mut pages = BTreeMap::<(usize, usize), PageRegion>::new();

    for seg in &parse.segments {
        if seg.memsz == 0 {
            continue;
        }

        let region = MemoryRegion::from_segment(seg, load_base);
        let start = region.va_start().as_usize();
        let end = region.va_end().as_usize();
        let file_end = region.vaddr + region.filesz;

        for page_va in (start..end).step_by(PAGE_SIZE) {
            let file_offset = region.file_offset_aligned() + (page_va - start);
            let covered_end = file_end.saturating_sub(page_va).min(PAGE_SIZE);
            let key = (page_va, file_offset);

            if let Some(existing) = pages.get_mut(&key) {
                existing.prot |= region.prot;
                existing.file_backed = existing.file_backed.max(covered_end);
            } else {
                pages.insert(
                    key,
                    PageRegion {
                        page_va,
                        file_offset,
                        file_backed: covered_end,
                        prot: region.prot,
                    },
                );
            }
        }
    }

    pages.into_values().collect()
}

// ---------------------------------------------------------------------------
// Auxv
// ---------------------------------------------------------------------------

/// Auxiliary vector entry for the user stack.
#[derive(Debug, Clone, Copy)]
pub struct AuxvEntry {
    pub atype: usize,
    pub value: usize,
}

const AT_NULL: usize = 0;
const AT_PAGESZ: usize = 6;
const AT_ENTRY: usize = 9;
const AT_PHDR: usize = 3;
const AT_PHENT: usize = 4;
const AT_PHNUM: usize = 5;
const AT_RANDOM: usize = 25;

// ---------------------------------------------------------------------------
// StackLayout — pure computation over argv / envp / auxv
// ---------------------------------------------------------------------------

/// Pre-computed user stack geometry.
///
/// Pure data: `build()` measures sizes; the IO shell in `exec.rs`
/// materialises this onto a physical page.
#[derive(Debug)]
pub struct StackLayout {
    /// Auxv entries (including AT_NULL terminator).
    pub auxv: Vec<AuxvEntry>,
    /// Number of pointer-table slots.
    pub n_slots: usize,
    /// Bytes needed for the pointer table.
    pub table_bytes: usize,
}

impl StackLayout {
    /// Compute the stack layout from argv, envp, and the pipeline's auxv.
    ///
    /// Pure function: no IO, no allocation beyond Vec.
    pub fn build(argv: &[String], envp: &[String], auxv: &[AuxvEntry]) -> Self {
        // Pointer table: argc + argv_ptrs + NULL + envp_ptrs + NULL + auxv*2
        let n_slots = 1 + argv.len() + 1 + envp.len() + 1 + auxv.len() * 2;
        let table_bytes = n_slots * core::mem::size_of::<usize>();
        Self {
            auxv: auxv.to_vec(),
            n_slots,
            table_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// ExecPipeline — the monad
// ---------------------------------------------------------------------------

/// The single accumulator threaded through every pipeline step.
///
/// Every `with_*` method consumes `self` and returns `Result<Self, Errno>`.
/// On error the partial state is dropped; the calling process's address
/// space has never been touched.
pub struct ExecPipeline {
    /// The address space under construction.
    pub vm: VmMap,

    /// Resolved user-space entry point (main binary or interpreter).
    pub entry: usize,

    /// Program break — highest segment end, page-aligned.
    pub brk: VirtAddr,

    /// Handle to the stack VmObject (set by `with_stack`).
    pub stack_obj: Option<Arc<crate::hal_common::LeveledRwLock<VmObject, 3>>>,

    /// Accumulated auxiliary vector entries.
    pub auxv: Vec<AuxvEntry>,
}

impl ExecPipeline {
    /// Create a fresh pipeline with an empty VmMap.
    pub fn new(pmap: Arc<SpinMutex<crate::mm::Pmap, 2>>) -> Self {
        Self {
            vm: VmMap::new(pmap),
            entry: 0,
            brk: VirtAddr(0),
            stack_obj: None,
            auxv: Vec::new(),
        }
    }

    /// Monadic bind — sugar for chaining fallible transforms.
    ///
    /// ```ignore
    /// pipeline
    ///     .and_then(|p| p.with_segments(&parse, id, path, 0))?
    ///     .and_then(|p| p.with_stack())?
    /// ```
    #[inline]
    pub fn and_then(self, f: impl FnOnce(Self) -> Result<Self, Errno>) -> Result<Self, Errno> {
        f(self)
    }

    // -----------------------------------------------------------------------
    // Pipeline steps
    // -----------------------------------------------------------------------

    /// Map all PT_LOAD segments from a parsed ELF into the VmMap.
    ///
    /// Relocates raw `ElfSegment`s by `load_base`, converts flags to
    /// `MapPerm`, handles overlapping page boundaries via
    /// `merge_file_coverage`, and inserts each VMA.
    ///
    /// Also sets `self.entry` (relocated) and `self.brk`, and pushes
    /// AT_ENTRY / AT_PHDR / AT_PHNUM / AT_PHENT / AT_PAGESZ into auxv.
    pub fn with_segments(
        mut self,
        parse: &ElfParseResult,
        vnode_id: usize,
        vnode_path: &str,
        load_base: usize,
    ) -> Result<Self, Errno> {
        // Convert raw segments to relocated MemoryRegions.
        let regions: Vec<MemoryRegion> = parse
            .segments
            .iter()
            .map(|seg| MemoryRegion::from_segment(seg, load_base))
            .collect();
        let page_regions = normalize_page_regions(parse, load_base);

        // Track max vaddr for brk.
        let mut max_vaddr: usize = 0;

        for page in page_regions {
            let obj = VmObject::new_vnode_region(
                vnode_id,
                vnode_path,
                1,
                page.file_offset,
                page.file_backed,
            );
            let mapping = VmMapping::FilePrivate {
                object: obj,
                offset: page.file_offset as u64,
            };

            self.vm
                .map_or_merge(
                    VirtAddr::new(page.page_va),
                    VirtAddr::new(page.page_va + PAGE_SIZE),
                    page.prot,
                    mapping,
                )
                .map_err(|_| {
                    kerr!(
                        exec,
                        warn,
                        Errno::Enoexec,
                        "exec: VMA insert failed (malformed ELF)"
                    )
                })?;
        }

        for region in regions.iter().filter(|r| r.memsz > 0) {
            let end = region.vaddr + region.memsz;
            if end > max_vaddr {
                max_vaddr = end;
            }
        }

        // Set entry, brk, and base auxv entries.
        self.entry = parse.entry as usize + load_base;
        self.brk = VirtAddr::new_page_aligned_up(max_vaddr);

        self.auxv.push(AuxvEntry {
            atype: AT_PAGESZ,
            value: PAGE_SIZE,
        });
        self.auxv.push(AuxvEntry {
            atype: AT_ENTRY,
            value: self.entry,
        });
        // Only emit AT_PHDR / AT_PHENT / AT_PHNUM when the ELF actually
        // contains a PT_PHDR segment.  A zero AT_PHDR poisons musl's
        // __init_libc (it dereferences the pointer to scan for PT_TLS),
        // so omitting the triple entirely is safer than sending 0.
        if parse.phdr_vaddr != 0 {
            self.auxv.push(AuxvEntry {
                atype: AT_PHDR,
                value: parse.phdr_vaddr as usize + load_base,
            });
            self.auxv.push(AuxvEntry {
                atype: AT_PHENT,
                value: parse.phentsize as usize,
            });
            self.auxv.push(AuxvEntry {
                atype: AT_PHNUM,
                value: parse.phnum as usize,
            });
        }

        Ok(self)
    }

    /// Layer an interpreter's PT_LOAD segments on top and override entry.
    ///
    /// Same as `with_segments` but deliberately overwrites `self.entry`
    /// with the interpreter's entry point (the main binary's entry was
    /// already recorded in auxv AT_ENTRY by the first `with_segments`).
    pub fn with_interp_segments(
        mut self,
        parse: &ElfParseResult,
        vnode_id: usize,
        vnode_path: &str,
        load_base: usize,
    ) -> Result<Self, Errno> {
        // Preserve the main binary's entry in auxv (already pushed).
        // Map the interpreter segments.
        let page_regions = normalize_page_regions(parse, load_base);

        for page in page_regions {
            let obj = VmObject::new_vnode_region(
                vnode_id,
                vnode_path,
                1,
                page.file_offset,
                page.file_backed,
            );
            let mapping = VmMapping::FilePrivate {
                object: obj,
                offset: page.file_offset as u64,
            };

            self.vm
                .map_or_merge(
                    VirtAddr::new(page.page_va),
                    VirtAddr::new(page.page_va + PAGE_SIZE),
                    page.prot,
                    mapping,
                )
                .map_err(|_| kerr!(exec, warn, Errno::Enoexec, "exec: interp VMA insert failed"))?;
        }

        // Override entry to the interpreter's entry point.
        self.entry = parse.entry as usize + load_base;

        Ok(self)
    }

    /// Attach an anonymous RW stack VMA and stash the backing object.
    ///
    /// The `Arc<VmObject>` is stored in `self.stack_obj` so the IO shell
    /// can later pass it to `materialize_stack` without a tuple return.
    pub fn with_stack(mut self) -> Result<Self, Errno> {
        let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
        let stack_obj = VmObject::new_anon(USER_STACK_SIZE);
        let stack_ref = Arc::clone(&stack_obj);

        self.vm
            .map(
                VirtAddr::new(stack_bottom),
                VirtAddr::new(USER_STACK_TOP),
                crate::map_perm!(R, W, U),
                VmMapping::AnonPrivate {
                    object: stack_obj,
                    offset: 0,
                },
            )
            .map_err(|_| kerr!(exec, error, Errno::Enomem, "exec: stack VMA insert failed"))?;

        self.stack_obj = Some(stack_ref);
        Ok(self)
    }

    /// Attach the sigcode trampoline VMA (infallible in a fresh address space).
    pub fn with_sigcode(mut self) -> Self {
        self.vm.map_sigcode();
        self
    }

    /// Finalize the auxv by appending AT_RANDOM (placeholder) and AT_NULL.
    ///
    /// Call this just before handing the pipeline to `materialize_stack`.
    /// AT_RANDOM's value is a placeholder (0); the IO shell fills in the
    /// actual virtual address after writing the random bytes.
    pub fn finalize_auxv(mut self) -> Self {
        self.auxv.push(AuxvEntry {
            atype: AT_RANDOM,
            value: 0, // patched by materialize_stack
        });
        self.auxv.push(AuxvEntry {
            atype: AT_NULL,
            value: 0,
        });
        self
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Compute merged file-backed byte coverage for overlapping PT_LOAD pages.
///
/// Pure fold — no IO, no VmMap mutation.
fn merge_file_coverage(regions: &[MemoryRegion]) -> BTreeMap<(usize, usize), usize> {
    regions
        .iter()
        .filter(|r| r.memsz > 0)
        .fold(BTreeMap::new(), |mut acc, r| {
            let entry = acc.entry(r.merge_key()).or_insert(0);
            if r.file_backed_bytes() > *entry {
                *entry = r.file_backed_bytes();
            }
            acc
        })
}
