//! Sv39 PTE encoding/decoding and flag definitions.
//!
//! Bit layout follows RISC-V Privileged Spec §4.4 (Sv39) and
//! FreeBSD `sys/riscv/include/pte.h` for RSW software bits.

#[cfg(target_arch = "loongarch64")]
pub use crate::hal::la64::paging::pte::{
    encode_pte, map_perm_to_pte_flags, pte_flags, pte_is_leaf, pte_is_valid, pte_pa, PteFlags,
};
#[cfg(target_arch = "riscv64")]
pub use crate::hal::rv64::paging::pte::{
    encode_pte, map_perm_to_pte_flags, pte_flags, pte_is_leaf, pte_is_valid, pte_pa, PteFlags,
};

#[cfg(all(test, feature = "qemu-test"))]
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

        let leaf_rwx = encode_pte(
            0x1000,
            PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::X,
        );
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
        let perm = crate::map_perm!(R, W);
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
        let perm = crate::map_perm!(R, X, U);
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
