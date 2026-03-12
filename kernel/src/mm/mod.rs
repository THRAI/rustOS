//! Memory management subsystem.

pub mod allocator;
pub mod pmap;
pub mod uio;
pub mod vm;

pub use allocator::{
    alloc_anon_sync, alloc_raw_frame_sync, frame_alloc_contiguous, frame_free,
    frame_free_contiguous, free_raw_frame, get_frame_meta, BuddyAllocator, Magazine, PageRole,
};
pub use pmap::{
    handle_shootdown_ipi, has_pending, pmap_activate, pmap_copy_page, pmap_create, pmap_deactivate,
    pmap_destroy, pmap_enter, pmap_extract, pmap_extract_with_flags, pmap_protect, pmap_remove,
    pmap_zero_page, Pmap, PteFlags,
};
pub use uio::{uiomove, UioDir};
pub use vm::{
    resolve_user_fault, BackingStore, EntryFlags, MapPerm, PageFaultAccessType, VmMap, VmMapEntry,
    VmObject, VmPage,
};
