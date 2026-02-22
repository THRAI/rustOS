//! BSD-style VmMap (address space) and VmArea (virtual memory region).
//!
//! VmMap uses `BTreeMap<VirtAddr, VmArea>` for O(log n) lookup of address
//! ranges. Each VmArea has a monotonic `AtomicU64` ID that is unique across
//! all VMAs, providing TOCTOU defense.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::RwLock;

use hal_common::VirtAddr;

use super::vm_object::VmObject;

// ---------------------------------------------------------------------------
// Monotonic VMA ID
// ---------------------------------------------------------------------------

static NEXT_VMA_ID: AtomicU64 = AtomicU64::new(1);

fn next_vma_id() -> u64 {
    NEXT_VMA_ID.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// MapPerm — page protection flags
// ---------------------------------------------------------------------------

bitflags::bitflags! {
    /// Page protection / permission flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MapPerm: u8 {
        const R = 1 << 0;
        const W = 1 << 1;
        const X = 1 << 2;
        const U = 1 << 3;
    }
}

// ---------------------------------------------------------------------------
// VmAreaType
// ---------------------------------------------------------------------------

/// Classification of a virtual memory area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmAreaType {
    Anonymous,
    FileBacked,
    Stack,
    Heap,
    Device,
}

// ---------------------------------------------------------------------------
// VmError
// ---------------------------------------------------------------------------

/// Errors from VM map operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    /// New VMA overlaps an existing one.
    Overlap,
    /// No VMA found at the given address.
    NotFound,
    /// The requested range is invalid (e.g., start >= end).
    InvalidRange,
}

// ---------------------------------------------------------------------------
// VmArea
// ---------------------------------------------------------------------------

/// A virtual memory area: a contiguous range of virtual addresses backed
/// by a VmObject.
pub struct VmArea {
    /// Monotonic unique ID (for TOCTOU defense).
    pub id: u64,
    /// Virtual address range [start, end).
    pub range: Range<VirtAddr>,
    /// Protection flags.
    pub prot: MapPerm,
    /// Backing VmObject.
    pub object: Arc<RwLock<VmObject>>,
    /// Offset into the VmObject where this mapping starts (in pages).
    pub obj_offset: u64,
    /// Type of this VMA.
    pub vma_type: VmAreaType,
}

impl VmArea {
    /// Create a new VmArea with an auto-assigned monotonic ID.
    pub fn new(
        range: Range<VirtAddr>,
        prot: MapPerm,
        object: Arc<RwLock<VmObject>>,
        obj_offset: u64,
        vma_type: VmAreaType,
    ) -> Self {
        Self {
            id: next_vma_id(),
            range,
            prot,
            object,
            obj_offset,
            vma_type,
        }
    }

    /// Check if this VMA contains the given virtual address.
    pub fn contains(&self, va: VirtAddr) -> bool {
        va >= self.range.start && va < self.range.end
    }
}

// ---------------------------------------------------------------------------
// VmMap
// ---------------------------------------------------------------------------

/// BSD-style address space map.
///
/// Holds all VMAs for a process, keyed by start address in a BTreeMap
/// for O(log n) lookup. The `pmap_root` field will hold the root page
/// table physical address once PMAP is implemented.
pub struct VmMap {
    /// VMAs keyed by start virtual address.
    areas: BTreeMap<VirtAddr, VmArea>,
    /// Root page table physical address (placeholder until PMAP, Plan 3).
    pub pmap_root: usize,
}

impl VmMap {
    /// Create an empty address space map.
    pub fn new() -> Self {
        Self {
            areas: BTreeMap::new(),
            pmap_root: 0,
        }
    }

    /// Find the VMA containing `va`, if any.
    pub fn find_area(&self, va: VirtAddr) -> Option<&VmArea> {
        // Find the last VMA whose start <= va, then check if va < end.
        self.areas
            .range(..=va)
            .next_back()
            .map(|(_, vma)| vma)
            .filter(|vma| vma.contains(va))
    }

    /// Find the VMA containing `va` (mutable), if any.
    pub fn find_area_mut(&mut self, va: VirtAddr) -> Option<&mut VmArea> {
        self.areas
            .range_mut(..=va)
            .next_back()
            .map(|(_, vma)| vma)
            .filter(|vma| vma.contains(va))
    }

    /// Insert a VMA, checking for overlap with existing areas.
    pub fn insert(&mut self, area: VmArea) -> Result<(), VmError> {
        if area.range.start >= area.range.end {
            return Err(VmError::InvalidRange);
        }
        // Check overlap: any existing VMA that starts before our end
        // and ends after our start.
        for (_, existing) in self.areas.range(..area.range.end) {
            if existing.range.end > area.range.start {
                return Err(VmError::Overlap);
            }
        }
        self.areas.insert(area.range.start, area);
        Ok(())
    }

    /// Remove the VMA containing `va`.
    pub fn remove(&mut self, va: VirtAddr) -> Option<VmArea> {
        // Find the key first, then remove.
        let key = self
            .areas
            .range(..=va)
            .next_back()
            .filter(|(_, vma)| vma.contains(va))
            .map(|(&k, _)| k);
        key.and_then(|k| self.areas.remove(&k))
    }

    /// Fork this address space for COW.
    ///
    /// For each Anonymous VMA, a new shadow VmObject is inserted so that
    /// both parent and child share the same backing pages. Write faults
    /// will later copy pages into the topmost shadow.
    ///
    /// Non-anonymous VMAs are cloned with a shared reference to the same
    /// VmObject (no shadow needed for file-backed, device, etc.).
    pub fn fork(&self) -> VmMap {
        let mut child = VmMap::new();
        for (_, vma) in &self.areas {
            let child_object = if vma.vma_type == VmAreaType::Anonymous {
                // Create a shadow for the child, pointing to the current object.
                let size = {
                    let obj = vma.object.read();
                    obj.size()
                };
                let shadow = VmObject::new_shadow(Arc::clone(&vma.object), size);
                shadow
            } else {
                // Shared reference (file-backed, device, etc.)
                Arc::clone(&vma.object)
            };

            let child_vma = VmArea::new(
                vma.range.clone(),
                vma.prot,
                child_object,
                vma.obj_offset,
                vma.vma_type,
            );
            // Safe: child is fresh, no overlaps possible.
            let _ = child.areas.insert(child_vma.range.start, child_vma);
        }
        child
    }

    /// Iterate all VMAs.
    pub fn iter(&self) -> impl Iterator<Item = &VmArea> {
        self.areas.values()
    }
}
