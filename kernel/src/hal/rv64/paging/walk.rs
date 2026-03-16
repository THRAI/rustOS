//! Generic page table walk parameterized by `LEVELS`.

use super::pte::{encode_pte, pte_is_leaf, pte_is_valid, pte_pa, PteFlags};
use crate::hal_common::{PhysAddr, VirtAddr};

#[inline]
pub fn vpn_index<const LEVELS: usize>(va: VirtAddr, level: usize) -> usize {
    let shift = 12 + 9 * (LEVELS - 1 - level);
    (va.as_usize() >> shift) & 0x1FF
}

pub unsafe fn walk<const LEVELS: usize>(
    root_pa: PhysAddr,
    va: VirtAddr,
    alloc: bool,
    allocator: &mut dyn FnMut(usize) -> Option<PhysAddr>,
) -> Option<*mut u64> {
    unsafe {
        let mut table_pa = root_pa.as_usize();

        for level in 0..LEVELS {
            let idx = vpn_index::<LEVELS>(va, level);
            let pte_ptr = (table_pa as *mut u64).add(idx);
            let pte = pte_ptr.read_volatile();

            if level == LEVELS - 1 {
                return Some(pte_ptr);
            }

            if pte_is_valid(pte) && pte_is_leaf(pte) {
                return Some(pte_ptr);
            }

            if pte_is_valid(pte) {
                table_pa = pte_pa(pte);
            } else if alloc {
                let new_page = allocator(level)?;
                pte_ptr.write_volatile(encode_pte(new_page.as_usize(), PteFlags::V));
                table_pa = new_page.as_usize();
            } else {
                return None;
            }
        }

        None
    }
}
