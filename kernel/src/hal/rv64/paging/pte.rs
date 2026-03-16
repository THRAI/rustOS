//! Sv39 PTE encoding/decoding and flag definitions.

use crate::mm::vm::MapPerm;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PteFlags: u64 {
        const V = 1 << 0;
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
        const G = 1 << 5;
        const A = 1 << 6;
        const D = 1 << 7;
        const SW_WIRED   = 1 << 8;
        const SW_MANAGED = 1 << 9;
    }
}

const PTE_PPN_SHIFT: u32 = 10;
const PTE_PPN_MASK: u64 = 0x003F_FFFF_FFFF_FC00;
const PTE_FLAGS_MASK: u64 = 0x3FF;

#[inline]
pub fn encode_pte(pa: usize, flags: PteFlags) -> u64 {
    ((pa as u64 >> 12) << PTE_PPN_SHIFT) | flags.bits()
}

#[inline]
pub fn pte_pa(raw: u64) -> usize {
    ((raw & PTE_PPN_MASK) >> PTE_PPN_SHIFT << 12) as usize
}

#[inline]
pub fn pte_flags(raw: u64) -> PteFlags {
    PteFlags::from_bits_truncate(raw & PTE_FLAGS_MASK)
}

#[inline]
pub fn pte_is_valid(raw: u64) -> bool {
    raw & PteFlags::V.bits() != 0
}

#[inline]
pub fn pte_is_leaf(raw: u64) -> bool {
    raw & (PteFlags::R | PteFlags::W | PteFlags::X).bits() != 0
}

pub fn map_perm_to_pte_flags(perm: MapPerm) -> PteFlags {
    let mut f = PteFlags::V | PteFlags::A | PteFlags::D;
    if perm.contains(MapPerm::R) {
        f |= PteFlags::R;
    }
    if perm.contains(MapPerm::W) {
        f |= PteFlags::W;
    }
    if perm.contains(MapPerm::X) {
        f |= PteFlags::X;
    }
    if perm.contains(MapPerm::U) {
        f |= PteFlags::U;
    }
    f
}
