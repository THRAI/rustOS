//! Pmap container types: typed views into the hardware page table.
//!
//! Models pmap as a Rust map container `VA → (PA, PteFlags)`.
//! `PmapRef` for read-only lookups, `PmapEntry` (Occupied/Vacant) for mutations.

use core::marker::PhantomData;

use super::{
    pte::{
        encode_pte, map_perm_to_pte_flags, pte_flags, pte_is_leaf, pte_is_valid, pte_pa, PteFlags,
    },
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
        #[cfg(target_arch = "riscv64")]
        {
            self.flags.contains(PteFlags::X)
        }
        #[cfg(target_arch = "loongarch64")]
        {
            !self.flags.contains(PteFlags::NX)
        }
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

// ---------------------------------------------------------------------------
// OccupiedEntryMut — lightweight mutable view for range traversal
// ---------------------------------------------------------------------------

/// Mutable entry yielded during range traversal.
///
/// Borrows the PTE slot directly via raw pointer, NOT `&mut Pmap` — this
/// avoids borrow conflicts with the walker that owns the iteration state.
pub struct OccupiedEntryMut<'a> {
    pub(crate) pte_ptr: *mut u64,
    pub(crate) va: VirtAddr,
    pub(crate) pa: PhysAddr,
    pub(crate) flags: PteFlags,
    pub(crate) _lifetime: PhantomData<&'a mut u64>,
}

impl OccupiedEntryMut<'_> {
    pub fn va(&self) -> VirtAddr {
        self.va
    }
    pub fn pa(&self) -> PhysAddr {
        self.pa
    }
    pub fn flags(&self) -> PteFlags {
        self.flags
    }

    /// Remove the PTE. Returns a token for batched flush.
    ///
    /// Does NOT update pmap stats or perform shootdown — the caller is
    /// responsible for collecting the token into a `ShootdownBatch`.
    pub fn remove(self) -> ShootdownToken {
        // SAFETY: `pte_ptr` is a valid, aligned pointer to a leaf PTE slot
        // obtained from the page table walker. The caller holds `&mut Pmap`
        // (via `for_each_in_range_mut`) which guarantees exclusive access.
        unsafe {
            self.pte_ptr.write_volatile(0);
        }
        ShootdownToken(self.va)
    }

    /// Change permissions on this PTE. Returns a token for batched flush.
    ///
    /// Preserves software bits (SW_WIRED, SW_MANAGED) from the original PTE.
    pub fn set_perm(&mut self, perm: MapPerm) -> ShootdownToken {
        let sw_bits = self.flags & (PteFlags::SW_WIRED | PteFlags::SW_MANAGED);
        let new_flags = map_perm_to_pte_flags(perm) | sw_bits;
        // SAFETY: same as `remove` — valid PTE pointer, exclusive pmap access.
        unsafe {
            self.pte_ptr
                .write_volatile(encode_pte(self.pa.as_usize(), new_flags));
        }
        self.flags = new_flags;
        ShootdownToken(self.va)
    }
}

// ---------------------------------------------------------------------------
// PmapRange — read-only range iterator over valid leaf PTEs
// ---------------------------------------------------------------------------

/// Read-only range iterator over valid leaf PTEs in `[start, end)`.
///
/// Skips empty subtrees in the Sv39 3-level page table:
/// - Invalid L0 PTE → skip 1 GiB
/// - Invalid L1 PTE → skip 2 MiB
///
/// Yields `(VirtAddr, PhysAddr, PteFlags)` for each valid leaf PTE.
pub struct PmapRange<'a> {
    _pmap: &'a super::Pmap,
    start_va: usize,
    end_va: usize,
    /// Per-level state: (table_pa, current_idx, end_idx_inclusive)
    stack: [(usize, usize, usize); 3],
    depth: usize,
}

impl<'a> PmapRange<'a> {
    /// Create a new range iterator. Called by `Pmap::range()`.
    pub(crate) fn new(
        pmap: &'a super::Pmap,
        start_va: usize,
        end_va: usize,
        root_stack_entry: (usize, usize, usize),
    ) -> Self {
        Self {
            _pmap: pmap,
            start_va,
            end_va,
            stack: [root_stack_entry, (0, 0, 0), (0, 0, 0)],
            depth: 0,
        }
    }

    /// Sv39 bit-shift for a given depth.
    /// depth 0 (L0/root) → 30, depth 1 (L1) → 21, depth 2 (L2) → 12.
    #[inline]
    fn shift_for(depth: usize) -> usize {
        12 + 9 * (2 - depth)
    }

    /// Reconstruct the virtual address from indices stored in the stack
    /// at levels `0..=depth`.
    #[inline]
    fn reconstruct_va(&self, depth: usize) -> usize {
        let mut va = 0usize;
        for d in 0..=depth {
            va |= self.stack[d].1 << Self::shift_for(d);
        }
        va
    }

    /// Compute child-level start/end indices when descending from `parent_depth`.
    ///
    /// Mirrors the clipping logic in `walk_range_mut`: compute the VA span
    /// of the parent entry's subtree, clip against `[start_va, end_va)`,
    /// then extract child-level VPN indices from the clipped range.
    #[inline]
    fn child_index_range(&self, parent_depth: usize) -> (usize, usize) {
        let child_shift = Self::shift_for(parent_depth + 1);
        let parent_span: usize = 1 << Self::shift_for(parent_depth);

        // VA base of the parent entry's subtree (from stack indices 0..=parent).
        let subtree_va = self.reconstruct_va(parent_depth);

        // Clip the iteration range to this subtree.
        let clipped_start = if self.start_va > subtree_va {
            self.start_va
        } else {
            subtree_va
        };
        let subtree_end = subtree_va + parent_span;
        let clipped_end = if self.end_va < subtree_end {
            self.end_va
        } else {
            subtree_end
        };

        let child_start = (clipped_start >> child_shift) & 0x1FF;
        let child_end = ((clipped_end - 1) >> child_shift) & 0x1FF;
        (child_start, child_end)
    }
}

impl Iterator for PmapRange<'_> {
    type Item = (VirtAddr, PhysAddr, PteFlags);

    fn next(&mut self) -> Option<Self::Item> {
        if self.start_va >= self.end_va {
            return None;
        }

        loop {
            let (table_pa, current_idx, end_idx) = self.stack[self.depth];

            // Past end at this level → pop up.
            if current_idx > end_idx {
                if self.depth == 0 {
                    return None; // All L0 entries exhausted.
                }
                self.depth -= 1;
                self.stack[self.depth].1 += 1; // Advance parent index.
                continue;
            }

            // SAFETY: `table_pa` is a valid, identity-mapped page table page PA.
            // `current_idx` is in [0, 512) so the offset is within the 4 KiB page.
            // We only read (no mutation), and hold `&Pmap` for lifetime safety.
            let raw = unsafe {
                let pte_ptr = (table_pa as *const u64).add(current_idx);
                pte_ptr.read_volatile()
            };

            if !pte_is_valid(raw) {
                // Invalid PTE → skip this subtree, advance index.
                self.stack[self.depth].1 += 1;
                continue;
            }

            if pte_is_leaf(raw) {
                // Valid leaf → yield (va, pa, flags), then advance.
                let va = self.reconstruct_va(self.depth);
                self.stack[self.depth].1 += 1;
                return Some((
                    VirtAddr::new(va),
                    PhysAddr::new(pte_pa(raw)),
                    pte_flags(raw),
                ));
            }

            // Valid non-leaf → descend if not at max depth.
            if self.depth < 2 {
                let child_table_pa = pte_pa(raw);
                let (child_start, child_end) = self.child_index_range(self.depth);
                self.depth += 1;
                self.stack[self.depth] = (child_table_pa, child_start, child_end);
                continue;
            }

            // Depth 2 but non-leaf — shouldn't happen in valid Sv39.
            // Skip defensively.
            self.stack[self.depth].1 += 1;
        }
    }
}
