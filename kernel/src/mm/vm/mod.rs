//! Virtual memory subsystem.
//!
//! Core data structures for BSD-style VM: VmObject (shadow chains, COW),
//! VmMap (address space), VmArea (virtual memory regions), and synchronous
//! page fault handler.

pub mod fault;
pub mod page;
pub mod vm_map;
pub mod vm_object;
pub mod wait_queue;

// fault_async and test_integration use kernel-only paths (crate::mm::, crate::fs::,
// crate::proc::) that do not exist in the kernel-mm test crate.
// kernel-mm sets feature "standalone" by default; the kernel crate does not.
#[cfg(not(feature = "standalone"))]
pub mod fault_async;

#[cfg(not(feature = "standalone"))]
pub mod test_integration;
