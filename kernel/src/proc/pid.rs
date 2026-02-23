//! PID allocation.
//!
//! Simple monotonic allocator using AtomicU32. PID 0 is reserved (idle),
//! PID 1 is init. First call returns 1.

use core::sync::atomic::{AtomicU32, Ordering};

/// Next PID to allocate. Starts at 1 (init).
static NEXT_PID: AtomicU32 = AtomicU32::new(1);

/// Allocate a new unique PID. Never returns 0.
pub fn alloc_pid() -> u32 {
    NEXT_PID.fetch_add(1, Ordering::Relaxed)
}
