//! Memory allocator subsystem.
//!
//! - `buddy`: Buddy system frame allocator with split/coalesce
//! - `magazine`: Per-CPU magazine cache for lock-free order-0 fast path
//! - `frame_allocator`: Dual API (async + sync) frame allocation

pub mod buddy;
pub mod magazine;

#[cfg(not(test))]
pub mod frame_allocator;

#[cfg(not(test))]
pub use frame_allocator::{
    init_frame_allocator,
    frame_alloc,
    frame_alloc_sync,
    frame_free,
    frame_alloc_contiguous,
    frame_free_contiguous,
    emergency_reclaim_sync,
    available_pages,
};

#[cfg(not(test))]
#[cfg(debug_assertions)]
pub use frame_allocator::STACK_CANARY;

/// Test-only stub: frame_alloc_sync backed by a simple atomic counter.
/// Returns fake PhysAddr values for unit testing fault handler logic.
#[cfg(test)]
pub fn frame_alloc_sync() -> Option<hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0x8000_0000);
    let addr = NEXT_FRAME.fetch_add(hal_common::PAGE_SIZE, Ordering::Relaxed);
    Some(hal_common::PhysAddr::new(addr))
}
