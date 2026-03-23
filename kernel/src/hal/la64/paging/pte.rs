use crate::mm::vm::MapPerm;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PteFlags: u64 {
        const V = 1 << 0;
        const A = 1 << 0;
        const D = 1 << 1;
        const PLV_USER = 0b11 << 2;
        const MAT_CC = 0b01 << 4;
        const G = 1 << 6;
        const PRESENT = 1 << 7;
        const W = 1 << 8;
        const M = 1 << 9;
        const NR = 1 << 61;
        const NX = 1 << 62;
        const RPLV = 1 << 63;

        const SW_WIRED   = 1 << 10;
        const SW_MANAGED = 1 << 11;
    }
}

const PTE_PFN_SHIFT: u32 = 12;
const PTE_PFN_MASK: u64 = ((1u64 << 48) - 1) & !((1u64 << PTE_PFN_SHIFT) - 1);
const PTE_FLAGS_MASK: u64 = !PTE_PFN_MASK;

#[inline]
pub fn encode_pte(pa: usize, flags: PteFlags) -> u64 {
    ((pa as u64) & PTE_PFN_MASK) | flags.bits()
}

#[inline]
pub fn pte_pa(raw: u64) -> usize {
    (raw & PTE_PFN_MASK) as usize
}

#[inline]
pub fn pte_flags(raw: u64) -> PteFlags {
    PteFlags::from_bits_truncate(raw & PTE_FLAGS_MASK)
}

#[inline]
pub fn pte_is_valid(raw: u64) -> bool {
    let flags = pte_flags(raw);
    flags.contains(PteFlags::V) && flags.contains(PteFlags::PRESENT)
}

#[inline]
pub fn pte_is_leaf(raw: u64) -> bool {
    let flags = pte_flags(raw);
    pte_is_valid(raw) && (flags & !(PteFlags::V | PteFlags::PRESENT)).bits() != 0
}

pub fn map_perm_to_pte_flags(perm: MapPerm) -> PteFlags {
    let mut f = PteFlags::V | PteFlags::A | PteFlags::PRESENT | PteFlags::MAT_CC;
    if perm.contains(MapPerm::W) {
        f |= PteFlags::W | PteFlags::D | PteFlags::M;
    }
    if !perm.contains(MapPerm::R) {
        f |= PteFlags::NR;
    }
    if !perm.contains(MapPerm::X) {
        f |= PteFlags::NX;
    }
    if perm.contains(MapPerm::U) {
        f |= PteFlags::PLV_USER | PteFlags::RPLV;
    }
    f
}
