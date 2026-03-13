//! Hashed wait queue for async/wake model integration with VmPage.
//!
//! Provides a globally locked hash table mapping a unique token (like the memory
//! address of a `VmPage`) to a list of async `Waker`s. This avoids bloating the
//! `VmPage` struct with `Vec<Waker>` directly.

use alloc::{collections::BTreeMap, vec::Vec};
use core::task::Waker;

use crate::hal_common::IrqSafeSpinLock;

/// Global wait queue hash table.
///
/// Lock ordering: **Level 9** (leaf lock).  Accessed from fault handlers
/// (VmPage busy-state release) and syscall context.  Never nests inside
/// any other lock.  IRQ-safe by type for future-proofing.
static WAIT_QUEUES: IrqSafeSpinLock<BTreeMap<usize, Vec<Waker>>, 9> =
    IrqSafeSpinLock::new(BTreeMap::new());

/// Extract and wake all registered `Waker`s associated with the given `token`.
/// This is typically called synchronously by `release_exclusive` and
/// `downgrade_exclusive_to_shared`.
pub fn wake_all(token: usize) {
    let mut queues = WAIT_QUEUES.lock();
    if let Some(waiters) = queues.remove(&token) {
        for waker in waiters {
            waker.wake();
        }
    }
}

/// Register a `Waker` to wait on a specific `token`.
/// Once registered, the caller should yield back to the async executor.
pub fn register_waker(token: usize, waker: Waker) {
    let mut queues = WAIT_QUEUES.lock();
    queues.entry(token).or_default().push(waker);
}

/// Remove a specifically registered waker (e.g., if a timeout occurs before wake_all).
/// Only removes one waker matching the given `Waker` reference.
pub fn remove_waker(token: usize, waker: &Waker) {
    let mut queues = WAIT_QUEUES.lock();
    if let Some(waiters) = queues.get_mut(&token) {
        if let Some(idx) = waiters.iter().position(|w| w.will_wake(waker)) {
            waiters.remove(idx);
        }
        if waiters.is_empty() {
            queues.remove(&token);
        }
    }
}
