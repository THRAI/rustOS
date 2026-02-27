//! PMAP layer: machine-dependent page table management.
//!
//! Bridges the MI VM subsystem (VmMap, VmObject, fault handler) and
//! the hardware Sv39 page tables. API matches BLACKBOX §4.3.

pub mod pte;
pub mod walk;
pub mod asid;
pub mod shootdown;
pub mod test_integration;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use hal_common::{PhysAddr, VirtAddr, PAGE_SIZE};

use self::pte::{
    PteFlags, encode_pte, pte_pa, pte_flags, pte_is_valid, pte_is_leaf, map_perm_to_pte_flags,
};
use super::vm::vm_map::MapPerm;

#[cfg(target_arch = "riscv64")]
use crate::executor::per_cpu::MAX_CPUS;
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
    /// Root page table physical address (page-aligned).
    root: PhysAddr,
    /// Current ASID.
    asid: u16,
    /// ASID generation (for lazy revalidation).
    generation: u64,
    /// Which CPUs have this pmap loaded in satp (FreeBSD pm_active).
    active: [AtomicBool; MAX_CPUS],
    /// All allocated page table pages (freed on destroy).
    pt_pages: Vec<PhysAddr>,
    /// Resident/wired page counts.
    stats: PmapStats,
}

// AtomicBool is Send+Sync, Vec<PhysAddr> is Send. Pmap is protected
// externally by the process lock or used from a single context.
unsafe impl Send for Pmap {}

impl Pmap {
    /// Create a dummy Pmap for unit tests (no real page table).
    #[cfg(test)]
    pub fn dummy() -> Self {
        const ATOMIC_FALSE: AtomicBool = AtomicBool::new(false);
        Pmap {
            root: PhysAddr::new(0),
            asid: 0,
            generation: 0,
            active: [ATOMIC_FALSE; MAX_CPUS],
            pt_pages: Vec::new(),
            stats: PmapStats::default(),
        }
    }

    /// Allocate a page table page via `frame_alloc_sync` and track it.
    fn alloc_pt_page(&mut self) -> Option<usize> {
        #[cfg(target_arch = "riscv64")]
        {
            let frame = super::allocator::frame_alloc_sync()?;
            // Zero the page (identity-mapped).
            pmap_zero_page(frame);
            self.pt_pages.push(frame);
            Some(frame.as_usize())
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// MI API (BLACKBOX §4.3)
// ---------------------------------------------------------------------------

/// Create a new address space with an empty root page table.
pub fn pmap_create() -> Pmap {
    #[cfg(target_arch = "riscv64")]
    let (root, extra_pt) = {
        let frame = super::allocator::frame_alloc_sync()
            .expect("pmap_create: out of memory for root PT");
        pmap_zero_page(frame);

        // Identity-map kernel RAM as a 1GB Sv39 gigapage at root entry 2.
        // Covers 0x8000_0000..0xC000_0000 which includes all kernel text,
        // data, stacks, and the frame allocator region.
        // Without this, switching satp to a user pmap unmaps the kernel.
        let root_ptr = frame.as_usize() as *mut u64;
        let kernel_giga_pte = pte::encode_pte(
            0x8000_0000,
            pte::PteFlags::V | pte::PteFlags::R | pte::PteFlags::W | pte::PteFlags::X
                | pte::PteFlags::A | pte::PteFlags::D | pte::PteFlags::G,
        );
        // Root entry index 2 covers VA 0x8000_0000..0xBFFF_FFFF
        unsafe { root_ptr.add(2).write(kernel_giga_pte); }

        // Map MMIO region (UART 0x1000_0000, virtio 0x1000_8000) via a
        // level-1 page table under root entry 0 (VA 0..0x3FFF_FFFF).
        // Use 2MB megapages for the 0x1000_0000..0x1020_0000 range.
        let l1_frame = super::allocator::frame_alloc_sync()
            .expect("pmap_create: out of memory for MMIO L1 PT");
        pmap_zero_page(l1_frame);
        let l1_ptr = l1_frame.as_usize() as *mut u64;

        // Root entry 0: non-leaf PTE pointing to L1 table
        let root0_pte = pte::encode_pte(l1_frame.as_usize(), pte::PteFlags::V);
        unsafe { root_ptr.add(0).write(root0_pte); }

        // L1 entry 128: 2MB megapage at 0x1000_0000 (UART + virtio-blk)
        // index = 0x1000_0000 >> 21 = 0x80 = 128
        let mmio_mega_pte = pte::encode_pte(
            0x1000_0000,
            pte::PteFlags::V | pte::PteFlags::R | pte::PteFlags::W
                | pte::PteFlags::A | pte::PteFlags::D | pte::PteFlags::G,
        );
        unsafe { l1_ptr.add(128).write(mmio_mega_pte); }

        // PLIC MMIO: 0x0C00_0000..0x0C40_0000 (priority regs + claim/complete).
        // L1 index 96 = 0x0C00_0000 >> 21, index 97 = 0x0C20_0000 >> 21.
        // Without this, any external interrupt while a user pmap is active
        // causes a kernel page fault when reading the PLIC claim register.
        let plic_flags = pte::PteFlags::V | pte::PteFlags::R | pte::PteFlags::W
            | pte::PteFlags::A | pte::PteFlags::D | pte::PteFlags::G;
        unsafe {
            l1_ptr.add(96).write(pte::encode_pte(0x0C00_0000, plic_flags));
            l1_ptr.add(97).write(pte::encode_pte(0x0C20_0000, plic_flags));
        }

        (frame, l1_frame)
    };
    #[cfg(not(target_arch = "riscv64"))]
    let (root, extra_pt) = (PhysAddr::new(0), PhysAddr::new(0));

    let (asid, generation) = asid::alloc_asid();

    const ATOMIC_FALSE: AtomicBool = AtomicBool::new(false);

    Pmap {
        root,
        asid,
        generation,
        active: [ATOMIC_FALSE; MAX_CPUS],
        pt_pages: {
            let mut v = Vec::new();
            #[cfg(target_arch = "riscv64")]
            {
                v.push(root);
                v.push(extra_pt);
            }
            v
        },
        stats: PmapStats::default(),
    }
}

/// Free all page table pages and release the ASID.
pub fn pmap_destroy(pmap: &mut Pmap) {
    #[cfg(target_arch = "riscv64")]
    {
        for &pa in &pmap.pt_pages {
            super::allocator::frame_free(pa);
        }
    }
    pmap.pt_pages.clear();
    pmap.stats.resident_count = 0;
    pmap.stats.wired_count = 0;
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
        let pte_ptr = walk::walk::<SV39_LEVELS>(
            pmap.root.as_usize(),
            va.as_usize(),
            true,
            &mut || pmap.alloc_pt_page(),
        )
        .ok_or(())?;

        let old = pte_ptr.read_volatile();
        let was_valid = pte_is_valid(old);

        pte_ptr.write_volatile(encode_pte(pa.as_usize(), flags));

        if was_valid {
            // Mapping changed: shootdown old TLB entry.
            #[cfg(target_arch = "riscv64")]
            shootdown::pmap_shootdown(
                &pmap.active,
                va.as_usize(),
                va.as_usize() + PAGE_SIZE,
                pmap.asid,
            );
        }

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
/// Walks each page in the range with alloc=false. Clears valid PTEs
/// and issues a shootdown for the entire range.
pub fn pmap_remove(pmap: &mut Pmap, va_start: VirtAddr, va_end: VirtAddr) {
    let mut invalidated = false;

    let mut va = va_start.as_usize();
    while va < va_end.as_usize() {
        unsafe {
            if let Some(pte_ptr) = walk::walk::<SV39_LEVELS>(
                pmap.root.as_usize(),
                va,
                false,
                &mut || None,
            ) {
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

    if invalidated {
        #[cfg(target_arch = "riscv64")]
        shootdown::pmap_shootdown(
            &pmap.active,
            va_start.as_usize(),
            va_end.as_usize(),
            pmap.asid,
        );
    }
}

/// Change protection on mappings in the range [va_start, va_end).
pub fn pmap_protect(pmap: &mut Pmap, va_start: VirtAddr, va_end: VirtAddr, prot: MapPerm) {
    let new_flags = map_perm_to_pte_flags(prot);
    let mut invalidated = false;

    let mut va = va_start.as_usize();
    while va < va_end.as_usize() {
        unsafe {
            if let Some(pte_ptr) = walk::walk::<SV39_LEVELS>(
                pmap.root.as_usize(),
                va,
                false,
                &mut || None,
            ) {
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

    if invalidated {
        #[cfg(target_arch = "riscv64")]
        shootdown::pmap_shootdown(
            &pmap.active,
            va_start.as_usize(),
            va_end.as_usize(),
            pmap.asid,
        );
    }
}

/// Translate a virtual address to a physical address.
pub fn pmap_extract(pmap: &Pmap, va: VirtAddr) -> Option<PhysAddr> {
    unsafe {
        let pte_ptr = walk::walk::<SV39_LEVELS>(
            pmap.root.as_usize(),
            va.as_usize(),
            false,
            &mut || None,
        )?;
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
        let pte_ptr = walk::walk::<SV39_LEVELS>(
            pmap.root.as_usize(),
            va.as_usize(),
            false,
            &mut || None,
        )?;
        let raw = pte_ptr.read_volatile();
        if pte_is_valid(raw) && pte_is_leaf(raw) {
            Some((PhysAddr::new(pte_pa(raw) | va.page_offset()), pte_flags(raw)))
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

    let cpu_id = crate::executor::per_cpu::current().cpu_id;
    pmap.active[cpu_id].store(true, Ordering::Release);

    // satp = Mode(8=Sv39) | ASID | PPN
    let satp: usize = (8usize << 60)
        | ((pmap.asid as usize) << 44)
        | (pmap.root.as_usize() >> 12);

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
    let cpu_id = crate::executor::per_cpu::current().cpu_id;
    pmap.active[cpu_id].store(false, Ordering::Release);
}

/// Handle A/D bit fault: set A and/or D bits on the PTE.
/// Returns true if the fault was resolved (PTE updated), false otherwise.
pub fn pmap_fault(pmap: &Pmap, va: VirtAddr, write: bool) -> bool {
    unsafe {
        let pte_ptr = match walk::walk::<SV39_LEVELS>(
            pmap.root.as_usize(),
            va.as_usize(),
            false,
            &mut || None,
        ) {
            Some(p) => p,
            None => return false,
        };
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

        if changed {
            pte_ptr.write_volatile(encode_pte(pte_pa(raw), flags));
        }
        changed
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
    let s = src.as_usize() as *const u8;
    let d = dst.as_usize() as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(s, d, PAGE_SIZE);
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
        let pte_ptr = match walk::walk::<SV39_LEVELS>(
            pmap.root.as_usize(),
            va.as_usize(),
            false,
            &mut || None,
        ) {
            Some(p) => p,
            None => return false,
        };
        let raw = pte_ptr.read_volatile();
        pte_is_valid(raw) && pte_flags(raw).contains(bit)
    }
}

fn pte_bit_clear(pmap: &mut Pmap, va: VirtAddr, bit: PteFlags) {
    unsafe {
        let pte_ptr = match walk::walk::<SV39_LEVELS>(
            pmap.root.as_usize(),
            va.as_usize(),
            false,
            &mut || None,
        ) {
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
