//! Virtual memory subsystem.
//!
//! Core data structures for BSD-style VM: VmObject (shadow chains, COW),
//! VmMap (address space), and VmArea (virtual memory regions).

pub mod vm_object;
pub mod vm_map;

pub use vm_object::{OwnedPage, PageOwnership, VmObject};
pub use vm_map::{MapPerm, VmArea, VmAreaType, VmError, VmMap};
