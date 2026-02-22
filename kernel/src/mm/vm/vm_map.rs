//! VmMap and VmArea — implemented in Task 2.
//!
//! BSD-style address space map with BTreeMap-based VMA tracking.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::RwLock;

use hal_common::VirtAddr;

use super::vm_object::VmObject;

/// Placeholder — full implementation in Task 2.
pub struct VmArea {
    _private: (),
}

/// Placeholder — full implementation in Task 2.
pub struct VmMap {
    _private: (),
}

/// VMA type classification.
pub enum VmAreaType {
    Anonymous,
    FileBacked,
    Stack,
    Heap,
    Device,
}

/// VM operation errors.
pub enum VmError {
    Overlap,
    NotFound,
    InvalidRange,
}
