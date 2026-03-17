//! Virtual memory subsystem.
//!
//! Core data structures for BSD-style VM: VmObject (shadow chains, COW),
//! VmMap (address space), and page fault handler.

pub mod fault;
pub mod map;
pub mod object;
pub mod page;

pub mod test_integration;

pub use fault::{
    async_resolve::resolve_user_fault, sync_fault_handler, FaultResult, PageFaultAccessType,
};
pub use map::{
    entry::{CowState, MapPerm, VmInherit, VmMapEntry, VmMapping},
    VmMap,
};
pub use object::{page_daemon, VObjIndex, VmObject, VnodePager};
pub use page::{
    register_waker, remove_waker, wake_all, ExclusiveBusyGuard, PageRef, SharedBusyGuard, VmPage,
};
