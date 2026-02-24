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

#[cfg(not(test))]
use crate::fs::vnode::Vnode;

/// Placeholder trait for test builds where the fs module is not available.
#[cfg(test)]
pub trait Vnode: Send + Sync {
    fn vnode_id(&self) -> u64;
    fn path(&self) -> &str;
}

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
    /// File-backed: vnode reference (None for anonymous).
    pub vnode: Option<Arc<dyn Vnode>>,
    /// File offset where this VMA's data starts.
    pub file_offset: u64,
    /// Number of bytes of file data in this VMA (rest is zero-fill).
    pub file_size: u64,
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
            vnode: None,
            file_offset: 0,
            file_size: 0,
        }
    }

    /// Create a file-backed VmArea for demand-paged ELF loading.
    pub fn new_file_backed(
        range: Range<VirtAddr>,
        prot: MapPerm,
        object: Arc<RwLock<VmObject>>,
        obj_offset: u64,
        vnode: Arc<dyn Vnode>,
        file_offset: u64,
        file_size: u64,
    ) -> Self {
        Self {
            id: next_vma_id(),
            range,
            prot,
            object,
            obj_offset,
            vma_type: VmAreaType::FileBacked,
            vnode: Some(vnode),
            file_offset,
            file_size,
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

    /// Find the VMA whose `range.end == va` and matches `vma_type` (mutable).
    /// Used by brk to locate the current heap VMA for in-place resize.
    pub fn find_area_ending_at_mut(&mut self, va: VirtAddr, ty: VmAreaType) -> Option<&mut VmArea> {
        // The VMA ending at `va` has start < va, so search backwards from va.
        self.areas
            .range_mut(..va)
            .next_back()
            .map(|(_, vma)| vma)
            .filter(|vma| vma.range.end == va && vma.vma_type == ty)
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
            let needs_shadow = matches!(
                vma.vma_type,
                VmAreaType::Anonymous | VmAreaType::Heap | VmAreaType::Stack
            );
            let child_object = if needs_shadow {
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

            let child_vma = VmArea {
                id: next_vma_id(),
                range: vma.range.clone(),
                prot: vma.prot,
                object: child_object,
                obj_offset: vma.obj_offset,
                vma_type: vma.vma_type,
                vnode: vma.vnode.clone(),
                file_offset: vma.file_offset,
                file_size: vma.file_size,
            };
            // Safe: child is fresh, no overlaps possible.
            let _ = child.areas.insert(child_vma.range.start, child_vma);
        }
        child
    }

    /// Remove all VMAs (used by exec to reset address space).
    pub fn clear(&mut self) {
        self.areas.clear();
    }

    /// Iterate all VMAs.
    pub fn iter(&self) -> impl Iterator<Item = &VmArea> {
        self.areas.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::vm_object::VmObject;

    fn make_vma(start: usize, end: usize) -> VmArea {
        let obj = VmObject::new((end - start) as usize);
        VmArea::new(
            VirtAddr::new(start)..VirtAddr::new(end),
            MapPerm::R | MapPerm::W,
            obj,
            0,
            VmAreaType::Anonymous,
        )
    }

    #[test]
    fn insert_and_find() {
        let mut map = VmMap::new();
        let vma = make_vma(0x1000, 0x3000);
        let id = vma.id;
        assert!(map.insert(vma).is_ok());
        let found = map.find_area(VirtAddr::new(0x1500)).unwrap();
        assert_eq!(found.id, id);
    }

    #[test]
    fn find_at_boundaries() {
        let mut map = VmMap::new();
        map.insert(make_vma(0x1000, 0x3000)).unwrap();
        // Start of range
        assert!(map.find_area(VirtAddr::new(0x1000)).is_some());
        // Last byte in range
        assert!(map.find_area(VirtAddr::new(0x2FFF)).is_some());
        // Just past end (exclusive)
        assert!(map.find_area(VirtAddr::new(0x3000)).is_none());
        // Before start
        assert!(map.find_area(VirtAddr::new(0x0FFF)).is_none());
    }

    #[test]
    fn insert_overlap_rejected() {
        let mut map = VmMap::new();
        map.insert(make_vma(0x1000, 0x3000)).unwrap();
        let result = map.insert(make_vma(0x2000, 0x4000));
        assert_eq!(result, Err(VmError::Overlap));
    }

    #[test]
    fn insert_adjacent_ok() {
        let mut map = VmMap::new();
        map.insert(make_vma(0x1000, 0x2000)).unwrap();
        // Adjacent, not overlapping
        assert!(map.insert(make_vma(0x2000, 0x3000)).is_ok());
    }

    #[test]
    fn remove_existing() {
        let mut map = VmMap::new();
        map.insert(make_vma(0x1000, 0x3000)).unwrap();
        assert!(map.remove(VirtAddr::new(0x1000)).is_some());
        assert!(map.find_area(VirtAddr::new(0x1500)).is_none());
    }

    #[test]
    fn remove_nonexistent() {
        let mut map = VmMap::new();
        assert!(map.remove(VirtAddr::new(0x1000)).is_none());
    }

    #[test]
    fn multiple_vmas() {
        let mut map = VmMap::new();
        map.insert(make_vma(0x1000, 0x2000)).unwrap();
        map.insert(make_vma(0x3000, 0x4000)).unwrap();
        map.insert(make_vma(0x5000, 0x6000)).unwrap();
        assert!(map.find_area(VirtAddr::new(0x1500)).is_some());
        assert!(map.find_area(VirtAddr::new(0x3500)).is_some());
        assert!(map.find_area(VirtAddr::new(0x5500)).is_some());
        assert!(map.find_area(VirtAddr::new(0x2500)).is_none()); // gap
    }

    #[test]
    fn fork_creates_shadows() {
        let mut parent_map = VmMap::new();
        let obj = VmObject::new(4096);
        {
            let mut w = obj.write();
            w.insert_page(0, super::super::vm_object::OwnedPage::new_anonymous(
                hal_common::PhysAddr::new(0xA000),
            ));
        }
        let vma = VmArea::new(
            VirtAddr::new(0x1000)..VirtAddr::new(0x2000),
            MapPerm::R | MapPerm::W,
            obj,
            0,
            VmAreaType::Anonymous,
        );
        parent_map.insert(vma).unwrap();

        let child_map = parent_map.fork();
        // Child should have a VMA at the same address
        let child_vma = child_map.find_area(VirtAddr::new(0x1000)).unwrap();
        let parent_vma = parent_map.find_area(VirtAddr::new(0x1000)).unwrap();
        // Child VMA should have a different ID (monotonic)
        assert_ne!(child_vma.id, parent_vma.id);
        // Child's object is a shadow (empty, but backed by parent's object)
        let child_obj = child_vma.object.read();
        assert_eq!(child_obj.resident_count(), 0);
    }

    #[test]
    fn vma_ids_are_unique() {
        let v1 = make_vma(0x1000, 0x2000);
        let v2 = make_vma(0x3000, 0x4000);
        let v3 = make_vma(0x5000, 0x6000);
        assert_ne!(v1.id, v2.id);
        assert_ne!(v2.id, v3.id);
        assert_ne!(v1.id, v3.id);
    }

    #[test]
    fn iter_all_vmas() {
        let mut map = VmMap::new();
        map.insert(make_vma(0x1000, 0x2000)).unwrap();
        map.insert(make_vma(0x3000, 0x4000)).unwrap();
        let count = map.iter().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn fork_shadow_has_backing_chain() {
        // Verify that after fork, child's VmObject has a backing pointer
        // that ultimately reaches the original parent object.
        let mut parent_map = VmMap::new();
        let obj = VmObject::new(8192);
        {
            let mut w = obj.write();
            w.insert_page(0, super::super::vm_object::OwnedPage::new_anonymous(
                hal_common::PhysAddr::new(0xDEAD_0000),
            ));
            w.insert_page(1, super::super::vm_object::OwnedPage::new_anonymous(
                hal_common::PhysAddr::new(0xBEEF_0000),
            ));
        }
        let vma = VmArea::new(
            VirtAddr::new(0x1000)..VirtAddr::new(0x3000),
            MapPerm::R | MapPerm::W,
            obj,
            0,
            VmAreaType::Anonymous,
        );
        parent_map.insert(vma).unwrap();

        let child_map = parent_map.fork();
        let child_vma = child_map.find_area(VirtAddr::new(0x1000)).unwrap();
        let child_obj = child_vma.object.read();
        // Shadow depth should be 1 (child shadow -> parent object)
        assert_eq!(child_obj.shadow_depth(), 1);
        // Parent's pages visible through shadow chain
        assert_eq!(child_obj.lookup_page(0).unwrap(), hal_common::PhysAddr::new(0xDEAD_0000));
        assert_eq!(child_obj.lookup_page(1).unwrap(), hal_common::PhysAddr::new(0xBEEF_0000));
    }

    #[test]
    fn fork_preserves_vma_count() {
        let mut map = VmMap::new();
        map.insert(make_vma(0x1000, 0x2000)).unwrap();
        map.insert(make_vma(0x3000, 0x4000)).unwrap();
        map.insert(make_vma(0x5000, 0x6000)).unwrap();
        let child = map.fork();
        assert_eq!(child.iter().count(), 3);
    }

    #[test]
    fn insert_invalid_range_rejected() {
        let mut map = VmMap::new();
        // start >= end should fail
        let obj = VmObject::new(4096);
        let vma = VmArea::new(
            VirtAddr::new(0x2000)..VirtAddr::new(0x1000),
            MapPerm::R,
            obj,
            0,
            VmAreaType::Anonymous,
        );
        assert!(map.insert(vma).is_err());
    }
}
