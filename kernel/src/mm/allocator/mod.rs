//! Memory allocator subsystem.
//!
//! - `buddy`: Buddy system frame allocator with split/coalesce
//! - `magazine`: Per-CPU magazine cache for lock-free order-0 fast path
//! - `frame_allocator`: Dual API (async + sync) frame allocation
//! - `types`: Strongly-typed frame wrappers (`TypedFrame`, `UserAnon`, `FileCache`)

pub mod buddy;
pub mod magazine;
pub mod types;

pub use types::{TypedFrame, FileCache, UserAnon};

#[cfg(not(test))]
pub mod frame_allocator;

#[cfg(not(test))]
pub use frame_allocator::{
    frame_alloc_contiguous, frame_alloc_sync, frame_free, frame_free_contiguous,
    init_frame_allocator,
};

/// Allocate an anonymous user page, wrapped in a `TypedFrame<UserAnon>`.
///
/// Uses `frame_alloc_sync` internally and returns a typed frame that
/// will automatically free the frame on Drop.
pub fn alloc_anon_sync() -> Option<types::TypedFrame<types::UserAnon>> {
    let phys = frame_alloc_sync()?;
    Some(types::TypedFrame {
        phys,
        _marker: core::marker::PhantomData,
    })
}

/// Test-only stub: frame_alloc_sync backed by a simple atomic counter.
/// Returns fake PhysAddr values for unit testing fault handler logic.
#[cfg(test)]
pub fn frame_alloc_sync() -> Option<hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0x8000_0000);
    let addr = NEXT_FRAME.fetch_add(hal_common::PAGE_SIZE, Ordering::Relaxed);
    Some(hal_common::PhysAddr::new(addr))
}

#[cfg(test)]
pub fn frame_free(_addr: hal_common::PhysAddr) {}

#[cfg(test)]
pub fn frame_alloc_contiguous(count: usize) -> Option<hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME_CONTIG: AtomicUsize = AtomicUsize::new(0x9000_0000);
    let addr = NEXT_FRAME_CONTIG.fetch_add(hal_common::PAGE_SIZE * (1 << count), Ordering::Relaxed);
    Some(hal_common::PhysAddr::new(addr))
}

#[cfg(test)]
pub fn frame_free_contiguous(_addr: hal_common::PhysAddr, _order: usize) {}

#[cfg(test)]
pub fn init_frame_allocator(_start: hal_common::PhysAddr, _end: hal_common::PhysAddr) {}
