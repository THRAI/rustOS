//! PMAP layer: machine-dependent page table management.
//!
//! Bridges the MI VM subsystem (VmMap, VmObject, fault handler) and
//! the hardware Sv39 page tables. API matches BLACKBOX §4.3.

pub mod asid;
pub mod container;
pub mod pte;
pub mod shootdown;
pub mod test_integration;
pub mod walk;

use alloc::vec::Vec;
use core::{
    array,
    sync::atomic::{AtomicBool, Ordering},
};

pub use pte::PteFlags;
pub(crate) use pte::{
    encode_pte, map_perm_to_pte_flags, pte_flags, pte_is_leaf, pte_is_valid, pte_pa,
};
pub use shootdown::{handle_shootdown_ipi, has_pending, ipi_broadcast_flush_all};

use super::vm::MapPerm;
#[cfg(target_arch = "riscv64")]
use crate::executor::MAX_CPUS;
use crate::{
    hal_common::{PhysAddr, VirtAddr, PAGE_SIZE},
    mm::{get_frame_meta, PageRole, VmPage},
};
#[cfg(not(target_arch = "riscv64"))]
const MAX_CPUS: usize = 8;

/// Sv39: 3-level page table.
const SV39_LEVELS: usize = 3;

/// Pmap statistics.
#[derive(Debug, Default)]
pub struct PmapStats {
    pub resident_count: usize,
    pub wired_count: usize,
}

/// Per-address-space page table state.
pub struct Pmap {
    /// Level-0 page tables (leaf PTEs)
    l0_tables: Vec<&'static VmPage>,
    /// Level-1 page tables (megapage directories)
    l1_directories: Vec<&'static VmPage>,
    /// Root page table physical address (page-aligned).
    root: &'static VmPage,
    /// Current ASID.
    asid: u16,
    /// ASID generation (for lazy revalidation).
    generation: u64,
    /// Which CPUs have this pmap loaded in satp (FreeBSD pm_active).
    active: [AtomicBool; MAX_CPUS],
    /// Resident/wired page counts.
    stats: PmapStats,
}

// AtomicBool is Send+Sync, Vec<PhysAddr> is Send. Pmap is protected
// externally by the process lock or used from a single context.
unsafe impl Send for Pmap {}

impl Pmap {
    /// Create a dummy Pmap for unit tests (no real page table).
    #[cfg(all(test, feature = "qemu-test"))]
    pub fn dummy() -> Self {
        const ATOMIC_FALSE: AtomicBool = AtomicBool::new(false);
        // Note: For dummy test objects without allocators, we conjure static VmPages here loosely,
        // but for safety in tests we should use properly allocated pages if needed. Just return uninit for now.
        // Using a leaked zeroed page as a placeholder since tests don't touch page tables.
        let dummy_root = Box::leak(Box::new(VmPage::new()));
        Pmap {
            l0_tables: Vec::new(),
            l1_directories: Vec::new(),
            root: dummy_root,
            asid: 0,
            generation: 0,
            active: array::from_fn(|_| AtomicBool::new(false)),
            stats: PmapStats::default(),
        }
    }

    /// Allocate a page table page via `alloc_pte_lX_sync` and track it.
    fn alloc_pt_page(&mut self, level: usize) -> Option<PhysAddr> {
        #[cfg(target_arch = "riscv64")]
        {
            if level == 0 {
                let frame =
                    super::allocator::alloc_pte_l1_sync().expect("failed to allocate L1 map");
                let pa = frame.phys();
                pmap_zero_page(pa);
                crate::klog!(vm, debug, "STUB: TraceEvent::Alloc {{ usage: PteL1 }}");
                self.l1_directories.push(frame);
                Some(pa)
            } else if level == 1 {
                let frame =
                    super::allocator::alloc_pte_l0_sync().expect("failed to allocate L0 map");
                let pa = frame.phys();
                pmap_zero_page(pa);
                crate::klog!(vm, debug, "STUB: TraceEvent::Alloc {{ usage: PteL0 }}");
                self.l0_tables.push(frame);
                Some(pa)
            } else {
                None
            }
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            None
        }
    }

    /// Read-only lookup. Returns `None` if no valid leaf PTE at `va`.
    pub fn get(&self, va: VirtAddr) -> Option<container::PmapRef> {
        // SAFETY: `self.root.phys()` is a valid page table root PA.
        // We pass `alloc = false` so no pages are allocated.
        // The kernel is identity-mapped so PA == VA for page table pages.
        unsafe {
            let pte_ptr = walk::walk::<SV39_LEVELS>(self.root.phys(), va, false, &mut |_| None)?;
            let raw = pte_ptr.read_volatile();
            if pte_is_valid(raw) && pte_is_leaf(raw) {
                Some(container::PmapRef {
                    pa: PhysAddr::new(pte_pa(raw)),
                    flags: pte_flags(raw),
                })
            } else {
                None
            }
        }
    }

    /// Mutable entry lookup, analogous to `HashMap::entry()`.
    ///
    /// Returns `Occupied` if a valid leaf PTE exists, `Vacant` otherwise.
    pub fn entry(&mut self, va: VirtAddr) -> container::PmapEntry<'_> {
        // SAFETY: same as `get` — valid root PA, no allocation, identity map.
        unsafe {
            let pte_ptr = walk::walk::<SV39_LEVELS>(self.root.phys(), va, false, &mut |_| None);
            match pte_ptr {
                Some(ptr) => {
                    let raw = ptr.read_volatile();
                    if pte_is_valid(raw) && pte_is_leaf(raw) {
                        container::PmapEntry::Occupied(container::OccupiedEntry {
                            pte_ptr: ptr,
                            pmap: self,
                            va,
                            pa: PhysAddr::new(pte_pa(raw)),
                            flags: pte_flags(raw),
                        })
                    } else {
                        container::PmapEntry::Vacant(container::VacantEntry { pmap: self, va })
                    }
                },
                None => container::PmapEntry::Vacant(container::VacantEntry { pmap: self, va }),
            }
        }
    }

    /// Insert only if vacant — the "cache fill" pattern.
    ///
    /// Returns `Ok(())` if the VA was already mapped (no-op) or if the
    /// new mapping was inserted successfully.
    pub fn entry_or_insert(&mut self, va: VirtAddr, pa: PhysAddr, perm: MapPerm) -> Result<(), ()> {
        match self.entry(va) {
            container::PmapEntry::Occupied(_) => Ok(()),
            container::PmapEntry::Vacant(v) => v.insert(pa, perm),
        }
    }
}

// ---------------------------------------------------------------------------
// MI API (BLACKBOX §4.3)
// ---------------------------------------------------------------------------

//TODO: make them impls of Pmap
/// Create a new address space with an empty root page table.
pub fn pmap_create() -> Pmap {
    #[cfg(target_arch = "riscv64")]
    let (root_frame, l1_frame) = {
        let frame =
            super::allocator::alloc_pte_l2_sync().expect("failed to allocate page table directory");
        pmap_zero_page(frame.phys());

        // Identity-map kernel RAM as a 1GB Sv39 gigapage at root entry 2.
        // Covers 0x8000_0000..0xC000_0000 which includes all kernel text,
        // data, stacks, and the frame allocator region.
        // Without this, switching satp to a user pmap unmaps the kernel.
        let root_ptr = frame.phys().as_usize() as *mut u64;
        let kernel_giga_pte = pte::encode_pte(
            0x8000_0000,
            pte::PteFlags::V
                | pte::PteFlags::R
                | pte::PteFlags::W
                | pte::PteFlags::X
                | pte::PteFlags::A
                | pte::PteFlags::D
                | pte::PteFlags::G,
        );
        // Root entry index 2 covers VA 0x8000_0000..0xBFFF_FFFF
        unsafe {
            root_ptr.add(2).write(kernel_giga_pte);
        }

        // Map MMIO region (UART 0x1000_0000, virtio 0x1000_8000) via a
        // level-1 page table under root entry 0 (VA 0..0x3FFF_FFFF).
        // Use 2MB megapages for the 0x1000_0000..0x1020_0000 range.
        let l1_frame = super::allocator::alloc_pte_l1_sync()
            .expect("pmap_create: out of memory for MMIO L1 PT");
        pmap_zero_page(l1_frame.phys());
        let l1_ptr = l1_frame.phys().as_usize() as *mut u64;

        // Root entry 0: non-leaf PTE pointing to L1 table
        let root0_pte = pte::encode_pte(l1_frame.phys().as_usize(), pte::PteFlags::V);
        unsafe {
            root_ptr.add(0).write(root0_pte);
        }

        // L1 entry 128: 2MB megapage at 0x1000_0000 (UART + virtio-blk)
        // index = 0x1000_0000 >> 21 = 0x80 = 128
        let mmio_mega_pte = pte::encode_pte(
            0x1000_0000,
            pte::PteFlags::V
                | pte::PteFlags::R
                | pte::PteFlags::W
                | pte::PteFlags::A
                | pte::PteFlags::D
                | pte::PteFlags::G,
        );
        unsafe {
            l1_ptr.add(128).write(mmio_mega_pte);
        }

        // PLIC MMIO: 0x0C00_0000..0x0C40_0000 (priority regs + claim/complete).
        // L1 index 96 = 0x0C00_0000 >> 21, index 97 = 0x0C20_0000 >> 21.
        // Without this, any external interrupt while a user pmap is active
        // causes a kernel page fault when reading the PLIC claim register.
        let plic_flags = pte::PteFlags::V
            | pte::PteFlags::R
            | pte::PteFlags::W
            | pte::PteFlags::A
            | pte::PteFlags::D
            | pte::PteFlags::G;
        unsafe {
            l1_ptr
                .add(96)
                .write(pte::encode_pte(0x0C00_0000, plic_flags));
            l1_ptr
                .add(97)
                .write(pte::encode_pte(0x0C20_0000, plic_flags));
        }

        (frame, l1_frame)
    };
    #[cfg(not(target_arch = "riscv64"))]
    let (root_frame, l1_frame) = {
        let dummy_root = Box::leak(Box::new(VmPage::new()));
        let dummy_l1 = Box::leak(Box::new(VmPage::new()));
        (dummy_root, dummy_l1)
    };

    let (asid, generation) = asid::alloc_asid();

    Pmap {
        l0_tables: Vec::new(),
        l1_directories: {
            #[cfg(target_arch = "riscv64")]
            let v = Vec::from([l1_frame]);
            #[cfg(not(target_arch = "riscv64"))]
            let v = Vec::new();
            v
        },
        root: root_frame,
        asid,
        generation,
        active: array::from_fn(|_| AtomicBool::new(false)),
        stats: PmapStats::default(),
    }
}

/// Free all page table pages and release the ASID.
pub fn pmap_destroy(pmap: &mut Pmap) {
    for frame in pmap.l0_tables.drain(..) {
        crate::mm::frame_free(frame);
    }
    for frame in pmap.l1_directories.drain(..) {
        crate::mm::frame_free(frame);
    }
    // Note: root is dropped explicitly. To ensure we don't double drop it if pmap is used,
    // we would need it wrapped in Option. However, pmap_destroy is typically only called right before Pmap is dropped natively.
    pmap.stats.resident_count = 0;
    pmap.stats.wired_count = 0;
}

impl Drop for Pmap {
    fn drop(&mut self) {
        let l0s = core::mem::take(&mut self.l0_tables);
        for frame in l0s {
            crate::mm::frame_free(frame);
        }
        let l1s = core::mem::take(&mut self.l1_directories);
        for frame in l1s {
            crate::mm::frame_free(frame);
        }
        // Since root is &'static mut, we can't nicely "take" it without Option or unsafe.
        // We will just fetch its phys address and free it directly to buddy allocator.
        // However, dummy objects might cause issues here if they were Box::leaked!
        // We should skip dummy objects if they have PA == 0.
        if self.root.phys_addr.as_usize() != 0 {
            let phys = self.root.phys_addr;
            if let Some(meta) = get_frame_meta(phys) {
                let old_ref = meta.dec_ref();
                debug_assert!(old_ref > 0, "pmap root frame refcount underflow");
                if old_ref == 1 {
                    meta.set_role(PageRole::Free);
                    crate::mm::free_raw_frame(phys);
                }
            }
        }
    }
}

/// Insert or update a mapping: va → pa with given protection.
///
/// Walks the page table with allocation enabled. Sets A/D bits upfront
/// to avoid hardware A/D faults on non-Svadu implementations.
pub fn pmap_enter(
    pmap: &mut Pmap,
    va: VirtAddr,
    pa: PhysAddr,
    prot: MapPerm,
    wired: bool,
) -> Result<(), ()> {
    let mut flags = map_perm_to_pte_flags(prot);
    if wired {
        flags |= PteFlags::SW_WIRED;
    }

    unsafe {
        let pte_ptr = walk::walk::<SV39_LEVELS>(pmap.root.phys(), va, true, &mut |level| {
            pmap.alloc_pt_page(level)
        })
        .ok_or(())?;

        let old = pte_ptr.read_volatile();
        let was_valid = pte_is_valid(old);

        pte_ptr.write_volatile(encode_pte(pa.as_usize(), flags));
        crate::klog!(
            vm,
            debug,
            "STUB: TraceEvent::PageFault {{ vaddr: {:#x} }}",
            va.0
        );

        // Always flush this VA: RISC-V may cache invalid translations, so
        // invalid->valid installs must also execute sfence.vma.
        #[cfg(target_arch = "riscv64")]
        shootdown::pmap_shootdown(
            &pmap.active,
            va.as_usize(),
            va.as_usize() + PAGE_SIZE,
            pmap.asid,
        );

        if !was_valid {
            pmap.stats.resident_count += 1;
        }
        if wired && !(was_valid && pte_flags(old).contains(PteFlags::SW_WIRED)) {
            pmap.stats.wired_count += 1;
        }
    }

    Ok(())
}

/// Remove mappings in the range [va_start, va_end).
///
/// Quiesces remote harts before modifying PTEs to prevent stale TLB
/// entries from being used during the modification window (C-4 fix).
pub fn pmap_remove(pmap: &mut Pmap, va_start: VirtAddr, va_end: VirtAddr) {
    // Quiesce remote harts BEFORE modifying any PTE.
    // If no remote harts are active, this is a no-op.
    #[cfg(target_arch = "riscv64")]
    let _quiesce = shootdown::QuiesceGuard::new(&pmap.active);

    let mut invalidated = false;

    let mut va = va_start.as_usize();
    while va < va_end.as_usize() {
        unsafe {
            if let Some(pte_ptr) =
                walk::walk::<SV39_LEVELS>(pmap.root.phys(), VirtAddr::new(va), false, &mut |_| None)
            {
                let old = pte_ptr.read_volatile();
                if pte_is_valid(old) {
                    pte_ptr.write_volatile(0);
                    pmap.stats.resident_count = pmap.stats.resident_count.saturating_sub(1);
                    if pte_flags(old).contains(PteFlags::SW_WIRED) {
                        pmap.stats.wired_count = pmap.stats.wired_count.saturating_sub(1);
                    }
                    invalidated = true;
                }
            }
        }
        va += PAGE_SIZE;
    }

    // Flush local TLB for the modified range.
    if invalidated {
        #[cfg(target_arch = "riscv64")]
        shootdown::adaptive_flush(va_start.as_usize(), va_end.as_usize(), pmap.asid as usize);
    }

    // _quiesce drops here → releases remote harts.
    // Remote harts flush_all and resume; next user-mode access walks fresh PTEs.
}

/// Change protection on mappings in the range [va_start, va_end).
///
/// Quiesces remote harts before modifying PTEs to prevent stale writable
/// TLB entries from being used during the modification window (C-4 fix).
/// This is critical for COW fork (downgrade RW→RO) and mprotect —
/// without quiesce, a remote hart could write through a stale writable
/// TLB entry while we're making the page read-only, corrupting shared data.
pub fn pmap_protect(pmap: &mut Pmap, va_start: VirtAddr, va_end: VirtAddr, prot: MapPerm) {
    let new_flags = map_perm_to_pte_flags(prot);

    // Quiesce remote harts BEFORE modifying any PTE.
    #[cfg(target_arch = "riscv64")]
    let _quiesce = shootdown::QuiesceGuard::new(&pmap.active);

    let mut invalidated = false;

    let mut va = va_start.as_usize();
    while va < va_end.as_usize() {
        unsafe {
            if let Some(pte_ptr) =
                walk::walk::<SV39_LEVELS>(pmap.root.phys(), VirtAddr::new(va), false, &mut |_| None)
            {
                let old = pte_ptr.read_volatile();
                if pte_is_valid(old) && pte_is_leaf(old) {
                    let pa = pte_pa(old);
                    // Preserve software bits from old PTE.
                    let sw_bits = pte_flags(old) & (PteFlags::SW_WIRED | PteFlags::SW_MANAGED);
                    pte_ptr.write_volatile(encode_pte(pa, new_flags | sw_bits));
                    invalidated = true;
                }
            }
        }
        va += PAGE_SIZE;
    }

    // Flush local TLB for the modified range.
    if invalidated {
        #[cfg(target_arch = "riscv64")]
        shootdown::adaptive_flush(va_start.as_usize(), va_end.as_usize(), pmap.asid as usize);
    }

    // _quiesce drops here → releases remote harts.
    // Remote harts flush_all and resume; next user-mode access walks fresh PTEs.
}

/// Translate a virtual address to a physical address.
pub fn pmap_extract(pmap: &Pmap, va: VirtAddr) -> Option<PhysAddr> {
    unsafe {
        let mut no_alloc = |_| None;
        let pte_ptr = walk::walk::<SV39_LEVELS>(pmap.root.phys(), va, false, &mut no_alloc)?;
        let raw = pte_ptr.read_volatile();
        if pte_is_valid(raw) && pte_is_leaf(raw) {
            Some(PhysAddr::new(pte_pa(raw) | va.page_offset()))
        } else {
            None
        }
    }
}

/// Extract physical address AND PTE flags for a mapped virtual address.
/// Returns None if the page is not mapped (no valid leaf PTE).
pub fn pmap_extract_with_flags(pmap: &Pmap, va: VirtAddr) -> Option<(PhysAddr, PteFlags)> {
    unsafe {
        let pte_ptr = walk::walk::<SV39_LEVELS>(pmap.root.phys(), va, false, &mut |_| None)?;
        let raw = pte_ptr.read_volatile();
        if pte_is_valid(raw) && pte_is_leaf(raw) {
            Some((
                PhysAddr::new(pte_pa(raw) | va.page_offset()),
                pte_flags(raw),
            ))
        } else {
            None
        }
    }
}

/// Activate this pmap on the current CPU: write satp, set pm_active.
///
/// If the pmap's generation is stale, re-allocate an ASID first.
#[cfg(target_arch = "riscv64")]
pub fn pmap_activate(pmap: &mut Pmap) {
    // Lazy ASID revalidation.
    if pmap.generation != asid::global_generation() {
        let (new_asid, new_gen) = asid::alloc_asid();
        pmap.asid = new_asid;
        pmap.generation = new_gen;
    }

    let cpu_id = crate::executor::current().cpu_id;
    pmap.active[cpu_id].store(true, Ordering::Release);

    let satp: usize =
        (8usize << 60) | ((pmap.asid as usize) << 44) | (pmap.root.phys().as_usize() >> 12);

    unsafe {
        core::arch::asm!(
            "csrw satp, {}",
            "sfence.vma zero, zero",
            in(reg) satp,
        );
    }
}

/// Deactivate this pmap on the current CPU: clear pm_active.
#[cfg(target_arch = "riscv64")]
pub fn pmap_deactivate(pmap: &mut Pmap) {
    let cpu_id = crate::executor::current().cpu_id;
    pmap.active[cpu_id].store(false, Ordering::Release);
    // SAFETY: Flush TLB entries for this ASID to prevent stale translations
    // when another hart reuses this pmap or its pages are freed.
    unsafe {
        core::arch::asm!(
            "sfence.vma zero, {asid}",
            asid = in(reg) pmap.asid as usize,
        );
    }
}

/// Handle A/D bit fault: set A and/or D bits on the PTE.
/// Returns true if the fault was resolved (PTE updated), false otherwise.
pub fn pmap_fault(pmap: &Pmap, va: VirtAddr, write: bool) -> bool {
    unsafe {
        let pte_ptr = match walk::walk::<SV39_LEVELS>(pmap.root.phys(), va, false, &mut |_| None) {
            Some(p) => p,
            None => return false,
        };

        loop {
            let raw = pte_ptr.read_volatile();
            if !pte_is_valid(raw) || !pte_is_leaf(raw) {
                return false;
            }

            let mut flags = pte_flags(raw);
            let mut changed = false;

            if !flags.contains(PteFlags::A) {
                flags |= PteFlags::A;
                changed = true;
            }
            if write && !flags.contains(PteFlags::D) {
                flags |= PteFlags::D;
                changed = true;
            }

            if !changed {
                return false; // already set
            }

            let new_raw = encode_pte(pte_pa(raw), flags);
            // SAFETY: pte_ptr is u64-aligned (page table entry). Reinterpret as AtomicU64
            // for lock-free CAS. Two harts setting same bits is benign.
            let pte_atomic = &*(pte_ptr as *const u64 as *const core::sync::atomic::AtomicU64);
            if pte_atomic
                .compare_exchange_weak(
                    raw,
                    new_raw,
                    core::sync::atomic::Ordering::Release,
                    core::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                return true;
            }
            // PTE changed under us — re-read
        }
    }
}

/// Check if the A (accessed) bit is set.
pub fn pmap_is_referenced(pmap: &Pmap, va: VirtAddr) -> bool {
    pte_bit_check(pmap, va, PteFlags::A)
}

/// Clear the A (accessed) bit.
pub fn pmap_clear_referenced(pmap: &mut Pmap, va: VirtAddr) {
    pte_bit_clear(pmap, va, PteFlags::A);
}

/// Check if the D (dirty) bit is set.
pub fn pmap_is_modified(pmap: &Pmap, va: VirtAddr) -> bool {
    pte_bit_check(pmap, va, PteFlags::D)
}

/// Clear the D (dirty) bit.
pub fn pmap_clear_modified(pmap: &mut Pmap, va: VirtAddr) {
    pte_bit_clear(pmap, va, PteFlags::D);
}

/// Zero a physical page (identity-mapped).
pub fn pmap_zero_page(pa: PhysAddr) {
    let ptr = pa.as_usize() as *mut u8;
    unsafe {
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
    }
}

/// Copy PAGE_SIZE bytes from src to dst (identity-mapped).
pub fn pmap_copy_page(src: PhysAddr, dst: PhysAddr) {
    unsafe {
        let src_va = src.into_kernel_vaddr();
        let dst_va = dst.into_kernel_vaddr();
        core::ptr::copy_nonoverlapping(src_va.as_ptr(), dst_va.as_mut_ptr(), PAGE_SIZE);
    }
}

/// Identity-map a PA range page-by-page into the pmap.
/// Snaps boundaries to page alignment (down for start, up for end).
pub fn pmap_enter_range(pmap: &mut Pmap, pa_start: usize, pa_end: usize, prot: MapPerm) {
    let start = pa_start & !(PAGE_SIZE - 1);
    let end = (pa_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let mut pa = start;
    while pa < end {
        pmap_enter(pmap, VirtAddr::new(pa), PhysAddr::new(pa), prot, false).unwrap();
        pa += PAGE_SIZE;
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn pte_bit_check(pmap: &Pmap, va: VirtAddr, bit: PteFlags) -> bool {
    unsafe {
        let pte_ptr = match walk::walk::<SV39_LEVELS>(pmap.root.phys(), va, false, &mut |_| None) {
            Some(p) => p,
            None => return false,
        };
        let raw = pte_ptr.read_volatile();
        pte_is_valid(raw) && pte_flags(raw).contains(bit)
    }
}

fn pte_bit_clear(pmap: &mut Pmap, va: VirtAddr, bit: PteFlags) {
    unsafe {
        let pte_ptr = match walk::walk::<SV39_LEVELS>(pmap.root.phys(), va, false, &mut |_| None) {
            Some(p) => p,
            None => return,
        };
        let raw = pte_ptr.read_volatile();
        if pte_is_valid(raw) {
            let flags = pte_flags(raw) & !bit;
            pte_ptr.write_volatile(encode_pte(pte_pa(raw), flags));

            #[cfg(target_arch = "riscv64")]
            shootdown::pmap_shootdown(
                &pmap.active,
                va.as_usize(),
                va.as_usize() + PAGE_SIZE,
                pmap.asid,
            );
        }
    }
}
