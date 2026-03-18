use super::pte::{encode_pte, pte_is_leaf, pte_is_valid, pte_pa, PteFlags};
use crate::hal_common::{PhysAddr, VirtAddr};

pub const LEVELS: usize = 4;
const INDEX_BITS: usize = 9;

#[inline]
pub fn vpn_index(va: VirtAddr, level: usize) -> usize {
    let shift = 12 + INDEX_BITS * (LEVELS - 1 - level);
    (va.as_usize() >> shift) & ((1 << INDEX_BITS) - 1)
}

pub unsafe fn walk(
    root_pa: PhysAddr,
    va: VirtAddr,
    alloc: bool,
    allocator: &mut dyn FnMut(usize) -> Option<PhysAddr>,
) -> Option<*mut u64> {
    unsafe {
        let mut table_pa = root_pa.as_usize();

        for level in 0..LEVELS {
            let idx = vpn_index(va, level);
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
                pte_ptr.write_volatile(encode_pte(
                    new_page.as_usize(),
                    PteFlags::V | PteFlags::PRESENT,
                ));
                table_pa = new_page.as_usize();
            } else {
                return None;
            }
        }

        None
    }
}
