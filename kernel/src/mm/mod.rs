//! Memory management subsystem.

pub mod allocator;
pub mod pmap;
pub mod uio;
pub mod vm;

pub use pmap::{
    handle_shootdown_ipi, has_pending, pmap_activate, pmap_copy_page, pmap_create, pmap_deactivate,
    pmap_destroy, pmap_enter, pmap_enter_range, pmap_extract, pmap_extract_with_flags, pmap_fault,
    pmap_protect, pmap_remove, pmap_zero_page, Pmap, PmapStats, PteFlags,
};
pub use uio::{chunk_size, uiomove, UioDir, UioResult};
pub use vm::{
    resolve_user_fault, BackingStore, EntryFlags, FaultError, FaultResult, MapPerm,
    PageFaultAccessType, VmError, VmMap, VmMapEntry, VmObject, VmPage, VnodePager,
};
