//! Memory management subsystem.

pub mod allocator;
pub mod pmap;
pub mod uio;
pub mod vm;

// ---------------------------------------------------------------------------
// Frame allocator gateway — pub(crate) wrappers for approved external callers.
//
// Raw frame allocation functions are pub(super) inside allocator/, meaning
// only code within mm/ can call them directly.  Modules outside mm/ (proc/,
// drivers/, main.rs) use these pub(crate) wrappers.  This creates a
// compiler-enforced boundary: adding a new external caller requires an
// explicit change here, visible in code review.
// ---------------------------------------------------------------------------

/// Boot-time initialization.  Called only from main.rs.
pub(crate) fn init_frame_allocator(
    start: crate::hal_common::PhysAddr,
    end: crate::hal_common::PhysAddr,
) {
    allocator::init_frame_allocator(start, end);
}

/// Allocate a raw frame with a specific role.
pub(crate) fn alloc_raw_frame_sync(
    role: allocator::PageRole,
) -> Option<crate::hal_common::PhysAddr> {
    allocator::alloc_raw_frame_sync(role)
}

/// Allocate an anonymous (zero-filled) frame.
pub(crate) fn alloc_anon_sync() -> Option<&'static vm::VmPage> {
    allocator::alloc_anon_sync()
}

/// Allocate contiguous frames (2^order pages).
pub(crate) fn frame_alloc_contiguous(order: usize) -> Option<crate::hal_common::PhysAddr> {
    allocator::frame_alloc_contiguous(order)
}

/// Free a single frame (refcount-aware).
pub(crate) fn frame_free(frame: &'static vm::VmPage) {
    allocator::frame_free(frame);
}

/// Free a raw frame by address.
pub(crate) fn free_raw_frame(addr: crate::hal_common::PhysAddr) {
    allocator::free_raw_frame(addr);
}

/// Free contiguous frames.
pub(crate) fn frame_free_contiguous(addr: crate::hal_common::PhysAddr, order: usize) {
    allocator::frame_free_contiguous(addr, order);
}

/// Get frame metadata by physical address.
pub(crate) fn get_frame_meta(addr: crate::hal_common::PhysAddr) -> Option<&'static vm::VmPage> {
    allocator::get_frame_meta(addr)
}

// Types that need to be visible crate-wide.
pub use allocator::{BuddyAllocator, Magazine, PageRole};
pub use pmap::{
    handle_shootdown_ipi, has_pending, pmap_activate, pmap_copy_page, pmap_create, pmap_deactivate,
    pmap_destroy, pmap_enter, pmap_extract, pmap_extract_with_flags, pmap_protect, pmap_remove,
    pmap_zero_page, Pmap, PteFlags,
};
pub use uio::{uiomove, UioDir};
pub use vm::{
    resolve_user_fault, CowState, MapPerm, PageFaultAccessType, VmMap, VmMapEntry, VmMapping,
    VmObject, VmPage,
};
