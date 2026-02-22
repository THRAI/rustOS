//! Memory allocator subsystem.
//!
//! - `buddy`: Buddy system frame allocator with split/coalesce
//! - `magazine`: Per-CPU magazine cache for lock-free order-0 fast path
//! - `frame_allocator`: Dual API (async + sync) frame allocation

pub mod buddy;
pub mod magazine;
pub mod frame_allocator;

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

#[cfg(debug_assertions)]
pub use frame_allocator::STACK_CANARY;
