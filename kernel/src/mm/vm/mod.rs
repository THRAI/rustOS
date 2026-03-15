//! Virtual memory subsystem.
//!
//! Core data structures for BSD-style VM: VmObject (shadow chains, COW),
//! VmMap (address space), VmArea (virtual memory regions), and synchronous
//! page fault handler.

pub mod fault;
pub mod map;
pub mod object;
pub mod page;
pub mod wait_queue;

// fault_async and test_integration use kernel-only paths (crate::mm::, crate::fs::,
// crate::proc::) that do not exist in the kernel-mm test crate.
// kernel-mm sets feature "standalone" by default; the kernel crate does not.
pub mod fault_async;

pub mod page_daemon;

pub mod test_integration;

pub use fault::{sync_fault_handler, FaultResult, PageFaultAccessType};
pub use fault_async::resolve_user_fault;
pub use map::{
    entry::{BackingStore, EntryFlags, MapPerm, VmMapEntry},
    VmMap,
};
pub use object::{VObjIndex, VmObject, VnodePager};
pub use page::{ExclusiveBusyGuard, SharedBusyGuard, VmPage};
pub use wait_queue::{register_waker, remove_waker, wake_all};
