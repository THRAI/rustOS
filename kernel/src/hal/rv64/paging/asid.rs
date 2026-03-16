//! Generation-based ASID allocator.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::hal_common::IrqSafeSpinLock;

pub const MAX_ASID: u16 = 65535;

pub static GLOBAL_GENERATION: AtomicU64 = AtomicU64::new(1);

struct AsidState {
    next: u16,
    generation: u64,
}

static ASID_ALLOC: IrqSafeSpinLock<AsidState, 9> = IrqSafeSpinLock::new(AsidState {
    next: 1,
    generation: 1,
});

pub fn alloc_asid() -> (u16, u64) {
    let mut state = ASID_ALLOC.lock();

    if state.next < MAX_ASID {
        let asid = state.next;
        state.next += 1;
        return (asid, state.generation);
    }

    state.generation = state.generation.wrapping_add(1);
    GLOBAL_GENERATION.store(state.generation, Ordering::Release);

    crate::hal::broadcast_flush_all();
    crate::hal::flush_all();

    state.next = 2;
    (1, state.generation)
}

#[inline]
pub fn global_generation() -> u64 {
    GLOBAL_GENERATION.load(Ordering::Acquire)
}
