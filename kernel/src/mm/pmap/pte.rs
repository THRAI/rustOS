//! Sv39 PTE encoding/decoding and flag definitions.
//!
//! Bit layout follows RISC-V Privileged Spec §4.4 (Sv39) and
//! FreeBSD `sys/riscv/include/pte.h` for RSW software bits.

use super::super::vm::vm_map::MapPerm;

bitflags::bitflags! {
    /// Sv39 page table entry flags (bits [9:0]).
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
        // RSW bits (software-defined, FreeBSD convention)
        const SW_WIRED   = 1 << 8;
        const SW_MANAGED = 1 << 9;
    }
}

const PTE_PPN_SHIFT: u32 = 10;
const PTE_PPN_MASK: u64 = 0x003F_FFFF_FFFF_FC00; // bits [53:10]
const PTE_FLAGS_MASK: u64 = 0x3FF; // bits [9:0]

/// Encode a PTE from a physical address and flags.
/// PA must be page-aligned. PPN occupies bits [53:10].
#[inline]
pub fn encode_pte(pa: usize, flags: PteFlags) -> u64 {
    ((pa as u64 >> 12) << PTE_PPN_SHIFT) | flags.bits()
}

/// Extract the physical address from a raw PTE (PPN << 12).
#[inline]
pub fn pte_pa(raw: u64) -> usize {
    ((raw & PTE_PPN_MASK) >> PTE_PPN_SHIFT << 12) as usize
}

/// Extract flags from a raw PTE (low 10 bits).
#[inline]
pub fn pte_flags(raw: u64) -> PteFlags {
    PteFlags::from_bits_truncate(raw & PTE_FLAGS_MASK)
}

/// Check if PTE is valid (V bit set).
#[inline]
pub fn pte_is_valid(raw: u64) -> bool {
    raw & PteFlags::V.bits() != 0
}

/// Check if PTE is a leaf (any of R|W|X set). Non-leaf PTEs point to
/// the next level page table and have only V set.
#[inline]
pub fn pte_is_leaf(raw: u64) -> bool {
    raw & (PteFlags::R | PteFlags::W | PteFlags::X).bits() != 0
}

/// Convert MI `MapPerm` to hardware `PteFlags`.
/// Always sets V and A|D (avoid hardware A/D faults on non-Svadu implementations).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let pa = 0x8020_0000usize;
        let flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;
        let raw = encode_pte(pa, flags);
        assert_eq!(pte_pa(raw), pa);
        assert_eq!(pte_flags(raw), flags);
    }

    #[test]
    fn encode_decode_all_flags() {
        let pa = 0x1000usize;
        let flags = PteFlags::all();
        let raw = encode_pte(pa, flags);
        assert_eq!(pte_pa(raw), pa);
        assert_eq!(pte_flags(raw), flags);
    }

    #[test]
    fn zero_pa() {
        let raw = encode_pte(0, PteFlags::V);
        assert_eq!(pte_pa(raw), 0);
        assert!(pte_is_valid(raw));
        assert!(!pte_is_leaf(raw)); // V only = non-leaf
    }

    #[test]
    fn leaf_detection() {
        let non_leaf = encode_pte(0x1000, PteFlags::V);
        assert!(!pte_is_leaf(non_leaf));

        let leaf_r = encode_pte(0x1000, PteFlags::V | PteFlags::R);
        assert!(pte_is_leaf(leaf_r));

        let leaf_x = encode_pte(0x1000, PteFlags::V | PteFlags::X);
        assert!(pte_is_leaf(leaf_x));

        let leaf_rwx = encode_pte(0x1000, PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::X);
        assert!(pte_is_leaf(leaf_rwx));
    }

    #[test]
    fn invalid_pte() {
        assert!(!pte_is_valid(0));
        assert!(!pte_is_leaf(0));
        assert_eq!(pte_pa(0), 0);
    }

    #[test]
    fn sw_bits_preserved() {
        let pa = 0x4000usize;
        let flags = PteFlags::V | PteFlags::R | PteFlags::SW_WIRED | PteFlags::SW_MANAGED;
        let raw = encode_pte(pa, flags);
        let extracted = pte_flags(raw);
        assert!(extracted.contains(PteFlags::SW_WIRED));
        assert!(extracted.contains(PteFlags::SW_MANAGED));
    }

    #[test]
    fn map_perm_conversion() {
        let perm = MapPerm::R | MapPerm::W;
        let flags = map_perm_to_pte_flags(perm);
        assert!(flags.contains(PteFlags::V));
        assert!(flags.contains(PteFlags::R));
        assert!(flags.contains(PteFlags::W));
        assert!(flags.contains(PteFlags::A));
        assert!(flags.contains(PteFlags::D));
        assert!(!flags.contains(PteFlags::X));
        assert!(!flags.contains(PteFlags::U));
    }

    #[test]
    fn map_perm_user_exec() {
        let perm = MapPerm::R | MapPerm::X | MapPerm::U;
        let flags = map_perm_to_pte_flags(perm);
        assert!(flags.contains(PteFlags::R));
        assert!(flags.contains(PteFlags::X));
        assert!(flags.contains(PteFlags::U));
        assert!(!flags.contains(PteFlags::W));
    }

    #[test]
    fn large_pa_roundtrip() {
        // Test with a PA that uses upper PPN bits
        let pa = 0x1_0000_0000usize; // 4 GiB
        let flags = PteFlags::V | PteFlags::R;
        let raw = encode_pte(pa, flags);
        assert_eq!(pte_pa(raw), pa);
    }
}
