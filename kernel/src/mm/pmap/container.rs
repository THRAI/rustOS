//! Pmap container types: typed views into the hardware page table.
//!
//! Models pmap as a Rust map container `VA → (PA, PteFlags)`.
//! `PmapRef` for read-only lookups, `PmapEntry` (Occupied/Vacant) for mutations.

use super::{
    pte::{encode_pte, map_perm_to_pte_flags, PteFlags},
    Pmap,
};
use crate::{
    hal_common::{PhysAddr, VirtAddr, PAGE_SIZE},
    mm::vm::MapPerm,
};

// ---------------------------------------------------------------------------
// ShootdownToken
// ---------------------------------------------------------------------------

/// Proof that a PTE was modified. Carries VA for flush.
pub struct ShootdownToken(pub(crate) VirtAddr);

impl ShootdownToken {
    pub fn va(&self) -> VirtAddr {
        self.0
    }
}

// ---------------------------------------------------------------------------
// PmapRef — read-only snapshot of a valid leaf PTE
// ---------------------------------------------------------------------------

/// Read-only view of a valid leaf PTE.
#[derive(Debug, Clone, Copy)]
pub struct PmapRef {
    pub(crate) pa: PhysAddr,
    pub(crate) flags: PteFlags,
}

impl PmapRef {
    pub fn pa(&self) -> PhysAddr {
        self.pa
    }
    pub fn flags(&self) -> PteFlags {
        self.flags
    }
    pub fn permits_write(&self) -> bool {
        self.flags.contains(PteFlags::W)
    }
    pub fn permits_exec(&self) -> bool {
        self.flags.contains(PteFlags::X)
    }
}

// ---------------------------------------------------------------------------
// OccupiedEntry — mutable view of an existing leaf PTE
// ---------------------------------------------------------------------------

/// Mutable view of an existing (valid leaf) PTE.
pub struct OccupiedEntry<'a> {
    pub(crate) pte_ptr: *mut u64,
    pub(crate) pmap: &'a mut Pmap,
    pub(crate) va: VirtAddr,
    pub(crate) pa: PhysAddr,
    pub(crate) flags: PteFlags,
}

impl<'a> OccupiedEntry<'a> {
    pub fn va(&self) -> VirtAddr {
        self.va
    }
    pub fn pa(&self) -> PhysAddr {
        self.pa
    }
    pub fn flags(&self) -> PteFlags {
        self.flags
    }

    /// Overwrite the mapping with a new PA and permissions, then shootdown.
    pub fn insert(&mut self, pa: PhysAddr, perm: MapPerm) {
        let new_flags = map_perm_to_pte_flags(perm);
        // Preserve software bits from old PTE.
        let sw_bits = self.flags & (PteFlags::SW_WIRED | PteFlags::SW_MANAGED);
        // SAFETY: `pte_ptr` is a valid, aligned pointer to a leaf PTE slot
        // obtained from a prior `walk()`. We hold `&mut Pmap` which guarantees
        // exclusive access to this address space's page tables.
        unsafe {
            self.pte_ptr
                .write_volatile(encode_pte(pa.as_usize(), new_flags | sw_bits));
        }
        self.pa = pa;
        self.flags = new_flags | sw_bits;

        #[cfg(target_arch = "riscv64")]
        super::shootdown::pmap_shootdown(
            &self.pmap.active,
            self.va.as_usize(),
            self.va.as_usize() + PAGE_SIZE,
            self.pmap.asid,
        );
    }

    /// Upgrade permissions (e.g. RO → RW for CoW resolution), then shootdown.
    pub fn promote(&mut self, new_perm: MapPerm) {
        let new_flags = map_perm_to_pte_flags(new_perm);
        // Preserve software bits from old PTE.
        let sw_bits = self.flags & (PteFlags::SW_WIRED | PteFlags::SW_MANAGED);
        // SAFETY: same as `insert` — valid PTE pointer, exclusive pmap access.
        unsafe {
            self.pte_ptr
                .write_volatile(encode_pte(self.pa.as_usize(), new_flags | sw_bits));
        }
        self.flags = new_flags | sw_bits;

        #[cfg(target_arch = "riscv64")]
        super::shootdown::pmap_shootdown(
            &self.pmap.active,
            self.va.as_usize(),
            self.va.as_usize() + PAGE_SIZE,
            self.pmap.asid,
        );
    }

    /// Remove the mapping: invalidate PTE, update stats, shootdown.
    ///
    /// Consumes the entry and returns a `ShootdownToken` as proof of
    /// invalidation.
    pub fn remove(self) -> ShootdownToken {
        let old_flags = self.flags;
        // SAFETY: same as `insert` — valid PTE pointer, exclusive pmap access.
        unsafe {
            self.pte_ptr.write_volatile(0);
        }

        self.pmap.stats.resident_count = self.pmap.stats.resident_count.saturating_sub(1);
        if old_flags.contains(PteFlags::SW_WIRED) {
            self.pmap.stats.wired_count = self.pmap.stats.wired_count.saturating_sub(1);
        }

        #[cfg(target_arch = "riscv64")]
        super::shootdown::pmap_shootdown(
            &self.pmap.active,
            self.va.as_usize(),
            self.va.as_usize() + PAGE_SIZE,
            self.pmap.asid,
        );

        ShootdownToken(self.va)
    }
}

// ---------------------------------------------------------------------------
// VacantEntry — handle to a VA with no valid leaf PTE
// ---------------------------------------------------------------------------

/// View of a vacant PTE slot (no valid leaf PTE at this VA).
pub struct VacantEntry<'a> {
    pub(crate) pmap: &'a mut Pmap,
    pub(crate) va: VirtAddr,
}

impl<'a> VacantEntry<'a> {
    /// Insert a new mapping, delegating to `pmap_enter`.
    pub fn insert(self, pa: PhysAddr, perm: MapPerm) -> Result<(), ()> {
        super::pmap_enter(self.pmap, self.va, pa, perm, false)
    }
}

// ---------------------------------------------------------------------------
// PmapEntry — Occupied | Vacant discriminated union
// ---------------------------------------------------------------------------

/// Entry API for pmap, analogous to `HashMap::Entry`.
pub enum PmapEntry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}
