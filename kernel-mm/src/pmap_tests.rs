//! Host-runnable tests for pmap PTE encoding/decoding and ASID allocator.
//!
//! These test the pure-logic portions of the pmap module without requiring
//! raw pointers or riscv64-specific hardware.

use hal_common::IrqSafeSpinLock;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::vm::vm_map::MapPerm;

// ---------------------------------------------------------------------------
// PTE flags and encoding (mirrored from kernel/src/mm/pmap/pte.rs)
// ---------------------------------------------------------------------------

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct PteFlags: u64 {
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

fn encode_pte(pa: usize, flags: PteFlags) -> u64 {
    ((pa as u64 >> 12) << PTE_PPN_SHIFT) | flags.bits()
}

fn pte_pa(raw: u64) -> usize {
    ((raw & PTE_PPN_MASK) >> PTE_PPN_SHIFT << 12) as usize
}

fn pte_flags(raw: u64) -> PteFlags {
    PteFlags::from_bits_truncate(raw & PTE_FLAGS_MASK)
}

fn pte_is_valid(raw: u64) -> bool {
    raw & PteFlags::V.bits() != 0
}

fn pte_is_leaf(raw: u64) -> bool {
    raw & (PteFlags::R | PteFlags::W | PteFlags::X).bits() != 0
}

fn map_perm_to_pte_flags(perm: MapPerm) -> PteFlags {
    let mut f = PteFlags::V | PteFlags::A | PteFlags::D;
    if perm.contains(MapPerm::R) { f |= PteFlags::R; }
    if perm.contains(MapPerm::W) { f |= PteFlags::W; }
    if perm.contains(MapPerm::X) { f |= PteFlags::X; }
    if perm.contains(MapPerm::U) { f |= PteFlags::U; }
    f
}

// ---------------------------------------------------------------------------
// ASID allocator (mirrored from kernel/src/mm/pmap/asid.rs)
// ---------------------------------------------------------------------------

const MAX_ASID: u16 = 65535;

static TEST_GLOBAL_GEN: AtomicU64 = AtomicU64::new(1);

/// Mutex to serialize ASID tests that share global mutable state.
/// Without this, parallel test execution causes data races on
/// TEST_ASID_ALLOC / TEST_GLOBAL_GEN.
static ASID_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct AsidState {
    next: u16,
    generation: u64,
}

static TEST_ASID_ALLOC: IrqSafeSpinLock<AsidState> = IrqSafeSpinLock::new(AsidState {
    next: 1,
    generation: 1,
});

fn test_alloc_asid() -> (u16, u64) {
    let mut state = TEST_ASID_ALLOC.lock();
    if state.next < MAX_ASID {
        let asid = state.next;
        state.next += 1;
        return (asid, state.generation);
    }
    // Rollover
    state.generation = state.generation.wrapping_add(1);
    TEST_GLOBAL_GEN.store(state.generation, Ordering::Release);
    // In real kernel: IPI broadcast flush here
    state.next = 2;
    (1, state.generation)
}

fn reset_asid_state() {
    let mut state = TEST_ASID_ALLOC.lock();
    state.next = 1;
    state.generation = 1;
    TEST_GLOBAL_GEN.store(1, Ordering::Release);
}

// ---------------------------------------------------------------------------
// VPN index (mirrored from kernel/src/mm/pmap/walk.rs)
// ---------------------------------------------------------------------------

fn vpn_index<const LEVELS: usize>(va: usize, level: usize) -> usize {
    let shift = 12 + 9 * (LEVELS - 1 - level);
    (va >> shift) & 0x1FF
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- PTE encode/decode --

    #[test]
    fn pte_encode_decode_roundtrip() {
        let pa = 0x8020_0000usize;
        let flags = PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;
        let raw = encode_pte(pa, flags);
        assert_eq!(pte_pa(raw), pa);
        assert_eq!(pte_flags(raw), flags);
    }

    #[test]
    fn pte_encode_decode_all_flags() {
        let pa = 0x1000usize;
        let flags = PteFlags::all();
        let raw = encode_pte(pa, flags);
        assert_eq!(pte_pa(raw), pa);
        assert_eq!(pte_flags(raw), flags);
    }

    #[test]
    fn pte_zero_pa() {
        let raw = encode_pte(0, PteFlags::V);
        assert_eq!(pte_pa(raw), 0);
        assert!(pte_is_valid(raw));
        assert!(!pte_is_leaf(raw));
    }

    #[test]
    fn pte_leaf_detection() {
        assert!(!pte_is_leaf(encode_pte(0x1000, PteFlags::V)));
        assert!(pte_is_leaf(encode_pte(0x1000, PteFlags::V | PteFlags::R)));
        assert!(pte_is_leaf(encode_pte(0x1000, PteFlags::V | PteFlags::X)));
        assert!(pte_is_leaf(encode_pte(0x1000, PteFlags::V | PteFlags::R | PteFlags::W | PteFlags::X)));
    }

    #[test]
    fn pte_invalid() {
        assert!(!pte_is_valid(0));
        assert!(!pte_is_leaf(0));
        assert_eq!(pte_pa(0), 0);
    }

    #[test]
    fn pte_sw_bits_preserved() {
        let pa = 0x4000usize;
        let flags = PteFlags::V | PteFlags::R | PteFlags::SW_WIRED | PteFlags::SW_MANAGED;
        let raw = encode_pte(pa, flags);
        let extracted = pte_flags(raw);
        assert!(extracted.contains(PteFlags::SW_WIRED));
        assert!(extracted.contains(PteFlags::SW_MANAGED));
    }

    #[test]
    fn pte_map_perm_conversion() {
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
    fn pte_map_perm_user_exec() {
        let perm = MapPerm::R | MapPerm::X | MapPerm::U;
        let flags = map_perm_to_pte_flags(perm);
        assert!(flags.contains(PteFlags::R));
        assert!(flags.contains(PteFlags::X));
        assert!(flags.contains(PteFlags::U));
        assert!(!flags.contains(PteFlags::W));
    }

    #[test]
    fn pte_large_pa_roundtrip() {
        let pa = 0x1_0000_0000usize; // 4 GiB
        let flags = PteFlags::V | PteFlags::R;
        let raw = encode_pte(pa, flags);
        assert_eq!(pte_pa(raw), pa);
    }

    // -- ASID allocator --

    #[test]
    fn asid_sequential_allocation() {
        let _guard = ASID_TEST_LOCK.lock().unwrap();
        reset_asid_state();
        let (a1, g1) = test_alloc_asid();
        let (a2, g2) = test_alloc_asid();
        let (a3, g3) = test_alloc_asid();
        assert_eq!(a1, 1);
        assert_eq!(a2, 2);
        assert_eq!(a3, 3);
        assert_eq!(g1, g2);
        assert_eq!(g2, g3);
    }

    #[test]
    fn asid_generation_rollover() {
        let _guard = ASID_TEST_LOCK.lock().unwrap();
        reset_asid_state();
        {
            let mut state = TEST_ASID_ALLOC.lock();
            state.next = MAX_ASID;
        }
        let (asid, gen) = test_alloc_asid();
        assert_eq!(asid, 1);
        assert_eq!(gen, 2);
        assert_eq!(TEST_GLOBAL_GEN.load(Ordering::Acquire), 2);

        let (asid2, gen2) = test_alloc_asid();
        assert_eq!(asid2, 2);
        assert_eq!(gen2, 2);
    }

    // -- VPN index --

    #[test]
    fn vpn_index_sv39_level0() {
        // Sv39: VA bits [38:30] for level 0
        let va = 0x1_C020_0000usize; // bits[38:30] = 7
        assert_eq!(vpn_index::<3>(va, 0), 7);
    }

    #[test]
    fn vpn_index_sv39_level1() {
        // Sv39: VA bits [29:21] for level 1
        let va = 0x0020_0000usize; // bits[29:21] = 1
        assert_eq!(vpn_index::<3>(va, 1), 1);
    }

    #[test]
    fn vpn_index_sv39_level2() {
        // Sv39: VA bits [20:12] for level 2
        let va = 0x0000_3000usize; // bits[20:12] = 3
        assert_eq!(vpn_index::<3>(va, 2), 3);
    }

    #[test]
    fn vpn_index_sv39_all_ones() {
        // Sv39: 3 VPN fields of 9 bits each = bits [38:12]
        // All VPN bits set: bits [38:12] = all 1s
        let va = 0x7F_FFFF_F000usize;
        assert_eq!(vpn_index::<3>(va, 0), 0x1FF);
        assert_eq!(vpn_index::<3>(va, 1), 0x1FF);
        assert_eq!(vpn_index::<3>(va, 2), 0x1FF);
    }

    #[test]
    fn vpn_index_sv39_zero() {
        assert_eq!(vpn_index::<3>(0, 0), 0);
        assert_eq!(vpn_index::<3>(0, 1), 0);
        assert_eq!(vpn_index::<3>(0, 2), 0);
    }

    #[test]
    fn asid_check_stale_after_rollover() {
        let _guard = ASID_TEST_LOCK.lock().unwrap();
        // Alloc an ASID in generation 1, force rollover to generation 2,
        // then verify the old ASID is stale (generation mismatch).
        reset_asid_state();
        let (asid, gen) = test_alloc_asid();
        assert_eq!(gen, 1);
        // Force rollover
        {
            let mut state = TEST_ASID_ALLOC.lock();
            state.next = MAX_ASID;
        }
        let (_new_asid, new_gen) = test_alloc_asid();
        assert_eq!(new_gen, 2);
        // The old ASID's generation (1) doesn't match global (2) — it's stale
        let global_gen = TEST_GLOBAL_GEN.load(Ordering::Acquire);
        assert_ne!(gen, global_gen);
        // A fresh alloc should be in the new generation
        let (fresh_asid, fresh_gen) = test_alloc_asid();
        assert_eq!(fresh_gen, 2);
        assert_ne!(fresh_asid, asid); // different ASID in new generation
    }

    #[test]
    fn asid_wraps_to_1_not_0() {
        let _guard = ASID_TEST_LOCK.lock().unwrap();
        // ASID 0 is reserved for kernel; after rollover, first ASID should be 1
        reset_asid_state();
        {
            let mut state = TEST_ASID_ALLOC.lock();
            state.next = MAX_ASID;
        }
        let (asid, _gen) = test_alloc_asid();
        assert_eq!(asid, 1); // not 0
    }

    #[test]
    fn pte_encode_decode_max_pa() {
        // Test with maximum physical address that fits in Sv39 PTE PPN field
        // 44-bit PPN * 4096 = 56-bit PA
        let pa = 0x00FF_FFFF_FFFF_F000usize;
        let flags = PteFlags::V | PteFlags::R | PteFlags::W;
        let raw = encode_pte(pa, flags);
        assert_eq!(pte_pa(raw), pa);
        assert_eq!(pte_flags(raw), flags);
    }
}
