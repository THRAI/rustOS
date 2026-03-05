//! Generation-based ASID allocator.
//!
//! Each pmap stores (asid, generation). On `pmap_activate`, if the pmap's
//! generation is stale, a new ASID is allocated. When the ASID space is
//! exhausted, the generation increments and all CPUs flush their TLBs.

use core::sync::atomic::{AtomicU64, Ordering};
use hal_common::IrqSafeSpinLock;

/// Maximum ASID value. Sv39/Sv48 on RV64 supports 16-bit ASIDs.
/// ASID 0 is reserved for the kernel.
pub const MAX_ASID: u16 = 65535;

/// Global generation counter. Read without lock for the fast-path check
/// in `pmap_activate`; only mutated under `ASID_ALLOC` lock.
pub static GLOBAL_GENERATION: AtomicU64 = AtomicU64::new(1);

struct AsidState {
    next: u16,
    generation: u64,
}

static ASID_ALLOC: IrqSafeSpinLock<AsidState> = IrqSafeSpinLock::new(AsidState {
    next: 1,
    generation: 1,
});

/// Allocate a fresh (asid, generation) pair.
///
/// Fast path: bump `next` under the lock.
/// Rollover path: increment generation, broadcast TLB flush to all CPUs,
/// reset `next` to 1.
pub fn alloc_asid() -> (u16, u64) {
    let mut state = ASID_ALLOC.lock();

    if state.next < MAX_ASID {
        let asid = state.next;
        state.next += 1;
        return (asid, state.generation);
    }

    // Rollover: generation bump + full TLB flush on all CPUs.
    state.generation = state.generation.wrapping_add(1);
    GLOBAL_GENERATION.store(state.generation, Ordering::Release);

    // Flush TLB on all CPUs. On the local CPU we flush directly;
    // remote CPUs are flushed via IPI broadcast.
    #[cfg(target_arch = "riscv64")]
    {
        super::shootdown::ipi_broadcast_flush_all();
        crate::hal::rv64::tlb::flush_all();
    }

    state.next = 2; // 1 is returned now
    (1, state.generation)
}

/// Read the current global generation (lock-free).
#[inline]
pub fn global_generation() -> u64 {
    GLOBAL_GENERATION.load(Ordering::Acquire)
}

#[cfg(all(test, feature = "qemu-test"))]
mod tests {
    use super::*;

    /// Reset allocator state for isolated tests.
    /// SAFETY: only call from single-threaded test context.
    fn reset_asid_state() {
        let mut state = ASID_ALLOC.lock();
        state.next = 1;
        state.generation = 1;
        GLOBAL_GENERATION.store(1, Ordering::Release);
    }

    #[test]
    fn sequential_allocation() {
        reset_asid_state();
        let (a1, g1) = alloc_asid();
        let (a2, g2) = alloc_asid();
        let (a3, g3) = alloc_asid();
        assert_eq!(a1, 1);
        assert_eq!(a2, 2);
        assert_eq!(a3, 3);
        assert_eq!(g1, g2);
        assert_eq!(g2, g3);
    }

    #[test]
    fn generation_increments_on_rollover() {
        reset_asid_state();
        // Exhaust the ASID space.
        {
            let mut state = ASID_ALLOC.lock();
            state.next = MAX_ASID;
        }
        let (asid, gen) = alloc_asid();
        // Should have rolled over: new generation, asid starts at 1.
        assert_eq!(asid, 1);
        assert_eq!(gen, 2);
        assert_eq!(global_generation(), 2);

        // Next allocation should be asid=2 in the new generation.
        let (asid2, gen2) = alloc_asid();
        assert_eq!(asid2, 2);
        assert_eq!(gen2, 2);
    }
}
