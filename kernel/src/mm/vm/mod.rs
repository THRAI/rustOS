//! Virtual memory subsystem.
//!
//! Core data structures for BSD-style VM: VmObject (shadow chains, COW),
//! VmMap (address space), VmArea (virtual memory regions), and synchronous
//! page fault handler.

pub mod vm_object;
pub mod vm_map;
pub mod fault;

#[cfg(not(test))]
pub mod test_integration;

pub use vm_object::{OwnedPage, PageOwnership, VmObject};
pub use vm_map::{MapPerm, VmArea, VmAreaType, VmError, VmMap};
pub use fault::{sync_fault_handler, FaultResult, FaultError, PageFaultAccessType};
