//! Generic page table walk parameterized by `LEVELS`.
//!
//! Sv39: LEVELS=3, VPN indices extracted via `vpn_index`.
//! The walk is identity-mapped: PA is used directly as a pointer.
//! A `phys_to_virt()` indirection will be added for higher-half kernel.

use super::pte::{encode_pte, pte_pa, pte_is_valid, pte_is_leaf, PteFlags};

/// Extract VPN index for a given level in an N-level page table.
///
/// Level 0 = root (highest VPN bits), Level LEVELS-1 = leaf (lowest VPN bits).
/// Sv39: shifts are [30, 21, 12] for levels [0, 1, 2].
#[inline]
pub fn vpn_index<const LEVELS: usize>(va: usize, level: usize) -> usize {
    let shift = 12 + 9 * (LEVELS - 1 - level);
    (va >> shift) & 0x1FF
}

/// Walk an N-level page table, optionally allocating intermediate pages.
///
/// Returns a pointer to the leaf PTE slot, or `None` if the walk fails
/// (missing intermediate table and `alloc` is false).
///
/// # Safety
///
/// - `root_pa` must be a valid, page-aligned physical address of a page table.
/// - The kernel must be identity-mapped (PA == VA for page table pages).
/// - `allocator` must return zeroed, page-aligned physical pages.
pub unsafe fn walk<const LEVELS: usize>(
    root_pa: usize,
    va: usize,
    alloc: bool,
    allocator: &mut dyn FnMut() -> Option<usize>,
) -> Option<*mut u64> {
    let mut table_pa = root_pa;

    for level in 0..LEVELS {
        let idx = vpn_index::<LEVELS>(va, level);
        let pte_ptr = (table_pa as *mut u64).add(idx);
        let pte = pte_ptr.read_volatile();

        // Last level: return pointer to this PTE slot.
        if level == LEVELS - 1 {
            return Some(pte_ptr);
        }

        // Superpage (leaf at non-last level): return pointer to this PTE.
        if pte_is_valid(pte) && pte_is_leaf(pte) {
            return Some(pte_ptr);
        }

        if pte_is_valid(pte) {
            // Non-leaf, valid: descend to next level.
            table_pa = pte_pa(pte);
        } else if alloc {
            // Invalid and alloc requested: allocate a new page table page.
            let new_page = allocator()?;
            // Write non-leaf PTE (V bit only, no R/W/X).
            pte_ptr.write_volatile(encode_pte(new_page, PteFlags::V));
            table_pa = new_page;
        } else {
            return None;
        }
    }

    None
}
