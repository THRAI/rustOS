//! Virtual memory subsystem.
//!
//! Core data structures for BSD-style VM: VmObject (shadow chains, COW),
//! VmMap (address space), VmArea (virtual memory regions), and synchronous
//! page fault handler.

pub mod fault;
pub mod vm_map;
pub mod vm_object;

#[cfg(not(test))]
pub mod test_integration;
