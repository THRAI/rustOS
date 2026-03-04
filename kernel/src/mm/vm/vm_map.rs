//! BSD-style VmMap (address space) and VmArea (virtual memory region).
//!
//! VmMap uses `BTreeMap<VirtAddr, VmArea>` for O(log n) lookup of address
//! ranges. Each VmArea has a monotonic `AtomicU64` ID that is unique across
//! all VMAs, providing TOCTOU defense.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal_common::addr::VirtPageNum;
use core::ops::Range;
use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use spin::RwLock;

use hal_common::{PAGE_SIZE, VirtAddr};

use crate::mm::vm::vm_object::VObjIndex;

use super::vm_object::VmObject;


#[cfg(not(test))]
// FIXME: dependency on fs
// use crate::fs::vnode::Vnode;
pub trait Vnode: Send + Sync {
    fn vnode_id(&self) -> u64;
    fn path(&self) -> &str;
}

/// Placeholder trait for test builds where the fs module is not available.
//TODO: adhere to Mach Vnode interface!
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
    //TODO: add flag packs like RW, RX, UW, etc.
}

// ---------------------------------------------------------------------------
// EntryFlags — VmMapEntry state flags
// ---------------------------------------------------------------------------

bitflags::bitflags! {
    /// VmMapEntry state flags for COW and concurrency control.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EntryFlags: u32 {
        const COW            = 1 << 0;
        const NEEDS_COPY     = 1 << 1;
        const GROWS_DOWN     = 1 << 2;
        const IN_TRANSITION  = 1 << 3;
        const NEEDS_WAKEUP   = 1 << 4;
    }
}

/// A declarative macro to combine MapPerm flags.
/// Usage: `map_perm!(R, W, U)`
#[macro_export]
macro_rules! map_perm {
    () => { $crate::mm::vm::vm_map::MapPerm::empty() };
    ($($flag:ident),+) => {
        $($crate::mm::vm::vm_map::MapPerm::$flag)|+
    };
}

// ---------------------------------------------------------------------------
// VmAreaType
// ---------------------------------------------------------------------------

/// Classification of a virtual memory area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmAreaType {
    Anonymous,
    SharedAnonymous,
    FileBacked,
    FileBackedPrivate,
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
//TODO: vmArea should be changed with pmap in vmMap.
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
    pub obj_offset: VirtPageNum,
    /// Type of this VMA.
    pub vma_type: VmAreaType,
    /// Entry state flags (COW, IN_TRANSITION, etc.).
    pub flags: EntryFlags,

    //TODO: file-backed Should be a vmObject concept, handled by VmObject Pager.
    /// File-backed: vnode reference (None for anonymous).
    pub vnode: Option<Arc<dyn Vnode>>,
    /// File offset where this VMA's data starts.
    pub file_offset: usize,
    /// Number of bytes of file data in this VMA (rest is zero-fill).
    pub file_size: usize,
}

impl VmArea {
    /// Create a new VmArea with an auto-assigned monotonic ID.
    pub fn new(
        range: Range<VirtAddr>,
        prot: MapPerm,
        object: Arc<RwLock<VmObject>>,
        obj_offset: VirtPageNum,
        vma_type: VmAreaType,
    ) -> Self {
        Self {
            id: next_vma_id(),
            range,
            prot,
            object,
            obj_offset,
            vma_type,
            flags: EntryFlags::empty(),
            vnode: None,
            file_offset: 0,
            file_size: 0,
        }
    }

    /// Create a file-backed VmArea for demand-paged ELF loading.
    //TODO: file-backed should be a vmObject concept, handled by VmObject Pager.
    pub fn new_file_backed(
        range: Range<VirtAddr>,
        prot: MapPerm,
        object: Arc<RwLock<VmObject>>,
        obj_offset:VirtPageNum ,
        vnode: Arc<dyn Vnode>,
        file_offset: usize,
        file_size: usize,
    ) -> Self {
        kprintln!("TODO: file-backed vma concepts to VmObject layer");
        Self {
            id: next_vma_id(),
            range,
            prot,
            object,
            obj_offset,
            vma_type: VmAreaType::FileBacked,
            flags: EntryFlags::empty(),
            vnode: Some(vnode),
            file_offset,
            file_size,
        }
    }

    /// Check if this VMA contains the given virtual address.
    pub fn contains(&self, va: VirtAddr) -> bool {
        va >= self.range.start && va < self.range.end
    }

    /// Calculate the offset into the VmObject for a given virtual address within this VMA.
    pub fn translate_to_obj_index(&self, va: VirtAddr) ->  VObjIndex {
        debug_assert!(self.contains(va));
        let delta_in_pages = (va - self.range.start).div_ceil(PAGE_SIZE);
        self.obj_offset + delta_in_pages
    }

    /// Coordinate transform: compute page index for a given virtual address.
    pub fn pindex_for(&self, vaddr: VirtAddr) -> VirtPageNum {
        debug_assert!(self.contains(vaddr));
        let delta_in_pages = (vaddr - self.range.start).div_ceil(PAGE_SIZE);
        self.obj_offset + delta_in_pages
    }

    /// Check if this entry is in transition (being modified by another thread).
    pub fn is_in_transition(&self) -> bool {
        self.flags.contains(EntryFlags::IN_TRANSITION)
    }

    /// Check if this entry needs copy-on-write materialization.
    pub fn needs_copy(&self) -> bool {
        self.flags.contains(EntryFlags::NEEDS_COPY)
    }

    /// Clear the NEEDS_COPY flag (requires write lock on VmMap).
    pub fn clear_needs_copy(&mut self) {
        self.flags.remove(EntryFlags::NEEDS_COPY);
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
    /// ABA defense timestamp (incremented on lock operations).
    pub timestamp: AtomicU32,
}

impl VmMap {
    /// Create an empty address space map.
    pub fn new() -> Self {
        Self {
            areas: BTreeMap::new(),
            pmap_root: 0,
            timestamp: AtomicU32::new(0),
        }
    }

    /// Try to upgrade read lock to write lock (stub - parking_lot RwLock doesn't support native upgrade).
    /// Returns true if upgrade succeeded, false if caller must release and re-acquire.
    /// NOTE: This is a no-op stub. Real implementation would require custom RwLock or external crate.
    pub fn try_upgrade_read_to_write(&self) -> bool {
        // parking_lot::RwLock doesn't support lock upgrade.
        // Caller must release read lock and acquire write lock manually.
        false
    }

    /// Downgrade write lock to read lock (stub - requires external lock handle).
    /// NOTE: This is a no-op stub. Real implementation would operate on RwLockWriteGuard.
    pub fn downgrade_write_to_read(&self) {
        // parking_lot::RwLock supports downgrade via RwLockWriteGuard::downgrade(),
        // but that requires the guard object. This stub documents the pattern.
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
                VmAreaType::Anonymous
                    | VmAreaType::FileBackedPrivate
                    | VmAreaType::Heap
                    | VmAreaType::Stack
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
                flags: vma.flags,
                vnode: vma.vnode.clone(),
                file_offset: vma.file_offset,
                file_size: vma.file_size,
            };
            // Safe: child is fresh, no overlaps possible.
            let _ = child.areas.insert(child_vma.range.start, child_vma);
        }
        child
    }

    /// Fork with independent VmObjects (deep copy, no shadows).
    ///
    /// Writable VMAs always get independent empty VmObjects -- `deep_copy_pages`
    /// will populate them from the parent's pmap.
    /// Read-only shared VMAs (FileBacked, Device, SharedAnonymous) get
    /// Arc::clone of the parent's VmObject so demand-paged pages remain
    /// accessible to the child.
    pub fn fork_deep_copy(&self) -> VmMap {
        let mut child = VmMap::new();
        for (_, vma) in &self.areas {
            // Any writable VMA needs its own VmObject (child will get copies
            // of the parent's pages, not shared references).
            // Read-only non-private VMAs can share the parent's VmObject.
            let needs_independent_copy = vma.prot.contains(MapPerm::W)
                || matches!(
                    vma.vma_type,
                    VmAreaType::Anonymous
                        | VmAreaType::FileBackedPrivate
                        | VmAreaType::Heap
                        | VmAreaType::Stack
                );
            let child_object = if needs_independent_copy {
                let size = vma.object.read().size();
                VmObject::new(size)
            } else {
                // Shared reference: read-only file-backed, device, shared-anonymous.
                // Pages already resident in the VmObject stay visible to child.
                Arc::clone(&vma.object)
            };
            let child_vma = VmArea {
                id: next_vma_id(),
                range: vma.range.clone(),
                prot: vma.prot,
                object: child_object,
                obj_offset: vma.obj_offset,
                vma_type: vma.vma_type,
                flags: vma.flags,
                vnode: vma.vnode.clone(),
                file_offset: vma.file_offset,
                file_size: vma.file_size,
            };
            let _ = child.areas.insert(child_vma.range.start, child_vma);
        }
        child
    }

    /// MMAP region: top-down allocation below stack.
    const MMAP_BASE: usize = 0x0000_003F_FFF0_0000; // just below USER_STACK_TOP
    const MMAP_MIN: usize = 0x0000_0020_0000_0000; // floor

    /// Find a free region of `size` bytes, searching top-down from MMAP_BASE.
    /// Returns the start address of the free region.
    pub fn find_free_area_topdown(&self, size: usize) -> Option<VirtAddr> {
        let aligned_size = (size + 0xFFF) & !0xFFF;
        if aligned_size == 0 {
            return None;
        }

        // Collect VMAs in the mmap region, sorted by start address
        let mmap_vmas: Vec<(usize, usize)> = self
            .areas
            .values()
            .filter(|vma| {
                vma.range.end.as_usize() > Self::MMAP_MIN
                    && vma.range.start.as_usize() < Self::MMAP_BASE
            })
            .map(|vma| (vma.range.start.as_usize(), vma.range.end.as_usize()))
            .collect();

        // Try gap between MMAP_BASE and highest VMA
        let mut ceiling = Self::MMAP_BASE;
        // Walk VMAs from highest to lowest
        for &(start, end) in mmap_vmas.iter().rev() {
            let gap_top = ceiling;
            let gap_bottom = end;
            if gap_top >= gap_bottom + aligned_size {
                let base = gap_top - aligned_size;
                if base >= Self::MMAP_MIN {
                    return Some(VirtAddr::new(base));
                }
            }
            ceiling = start;
        }

        // Try gap between lowest VMA and MMAP_MIN
        if ceiling >= Self::MMAP_MIN + aligned_size {
            let base = ceiling - aligned_size;
            if base >= Self::MMAP_MIN {
                return Some(VirtAddr::new(base));
            }
        }

        None
    }

    /// Remove all VMAs overlapping [start, end). Split VMAs that partially overlap.
    /// Returns Vec of removed VmAreas for frame cleanup.
    pub fn remove_range(&mut self, start: VirtAddr, end: VirtAddr) -> Vec<VmArea> {
        if start >= end {
            return Vec::new();
        }

        // Collect keys of all VMAs that overlap [start, end)
        let overlapping: Vec<VirtAddr> = self
            .areas
            .range(..end)
            .filter(|(_, vma)| vma.range.end > start)
            .map(|(&k, _)| k)
            .collect();

        let mut removed = Vec::new();

        for key in overlapping {
            let vma = self.areas.remove(&key).unwrap();

            let vma_start = vma.range.start;
            let vma_end = vma.range.end;

            if vma_start >= start && vma_end <= end {
                // Fully contained: remove entirely
                removed.push(vma);
            } else if vma_start < start && vma_end > end {
                // Spans both sides: split into left + right, remove middle
                let left_size = start.as_usize() - vma_start.as_usize();
                let right_start = end;

                // Left portion
                let left_obj = VmObject::new(left_size);
                let left_vma = VmArea {
                    id: next_vma_id(),
                    range: vma_start..start,
                    prot: vma.prot,
                    object: left_obj,
                    obj_offset: vma.obj_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset,
                    file_size:
                        if vma.file_size > left_size { left_size } else { vma.file_size },
                };
                self.areas.insert(vma_start, left_vma);

                // Right portion
                let right_size = vma_end.as_usize() - right_start.as_usize();
                let right_obj = VmObject::new(right_size);
                let right_offset_delta = right_start.as_usize() - vma_start.as_usize();
                let right_vma = VmArea {
                    id: next_vma_id(),
                    range: right_start..vma_end,
                    prot: vma.prot,
                    object: right_obj,
                    obj_offset: vma.obj_offset + right_offset_delta,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset + right_offset_delta,
                    file_size: vma.file_size.saturating_sub(right_offset_delta),
                };
                self.areas.insert(right_start, right_vma);

                // The middle portion is "removed"
                removed.push(vma);
            } else if vma_start < start {
                // Partial overlap at end: truncate VMA to end at `start`
                let keep_size = start.as_usize() - vma_start.as_usize();
                let truncated_obj = VmObject::new(keep_size);
                let kept = VmArea {
                    id: next_vma_id(),
                    range: vma_start..start,
                    prot: vma.prot,
                    object: truncated_obj,
                    obj_offset: vma.obj_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset,
                    file_size: if vma.file_size > keep_size { keep_size } else { vma.file_size },
                };
                self.areas.insert(vma_start, kept);
                removed.push(vma);
            } else {
                // Partial overlap at start: truncate VMA to start at `end`
                let new_start = end;
                let new_size = vma_end.as_usize() - new_start.as_usize();
                let truncated_obj = VmObject::new(new_size);
                let offset_delta = new_start.as_usize() - vma_start.as_usize();
                let kept = VmArea {
                    id: next_vma_id(),
                    range: new_start..vma_end,
                    prot: vma.prot,
                    object: truncated_obj,
                    obj_offset: vma.obj_offset + offset_delta,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset + offset_delta,
                    file_size: vma.file_size.saturating_sub(offset_delta),
                };
                self.areas.insert(new_start, kept);
                removed.push(vma);
            }
        }

        removed
    }

    /// Change permissions on all VMAs overlapping [start, end).
    /// Splits VMAs at boundaries if the range doesn't align.
    pub fn protect_range(&mut self, start: VirtAddr, end: VirtAddr, new_prot: MapPerm) {
        if start >= end {
            return;
        }

        // Collect keys of overlapping VMAs
        let overlapping: Vec<VirtAddr> = self
            .areas
            .range(..end)
            .filter(|(_, vma)| vma.range.end > start)
            .map(|(&k, _)| k)
            .collect();

        for key in overlapping {
            let vma = self.areas.remove(&key).unwrap();
            let vma_start = vma.range.start;
            let vma_end = vma.range.end;

            if vma_start >= start && vma_end <= end {
                // Fully contained: just update prot
                let mut updated = vma;
                updated.prot = new_prot;
                self.areas.insert(vma_start, updated);
            } else if vma_start < start && vma_end > end {
                // Spans both sides: split into 3 (left unchanged, middle new prot, right unchanged)
                let left_size = start.as_usize() - vma_start.as_usize();
                let left = VmArea {
                    id: next_vma_id(),
                    range: vma_start..start,
                    prot: vma.prot,
                    object: VmObject::new(left_size),
                    obj_offset: vma.obj_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset,
                    file_size: core::cmp::min(vma.file_size, left_size),
                };
                self.areas.insert(vma_start, left);

                let mid_size = end.as_usize() - start.as_usize();
                let mid_offset = start.as_usize() - vma_start.as_usize();
                let mid = VmArea {
                    id: next_vma_id(),
                    range: start..end,
                    prot: new_prot,
                    object: VmObject::new(mid_size),
                    obj_offset: vma.obj_offset + mid_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset + mid_offset,
                    file_size: vma
                        .file_size
                        .saturating_sub(mid_offset)
                        .min(mid_size),
                };
                self.areas.insert(start, mid);

                let right_start = end;
                let right_size = vma_end.as_usize() - right_start.as_usize();
                let right_offset = right_start.as_usize() - vma_start.as_usize();
                let right = VmArea {
                    id: next_vma_id(),
                    range: right_start..vma_end,
                    prot: vma.prot,
                    object: VmObject::new(right_size),
                    obj_offset: vma.obj_offset + right_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset + right_offset,
                    file_size: vma.file_size.saturating_sub(right_offset),
                };
                self.areas.insert(right_start, right);
            } else if vma_start < start {
                // Partial overlap at end: split at `start`
                let left_size = start.as_usize() - vma_start.as_usize();
                let left = VmArea {
                    id: next_vma_id(),
                    range: vma_start..start,
                    prot: vma.prot,
                    object: VmObject::new(left_size),
                    obj_offset: vma.obj_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset,
                    file_size: core::cmp::min(vma.file_size, left_size),
                };
                self.areas.insert(vma_start, left);

                let right_offset = start.as_usize() - vma_start.as_usize();
                let right_size = vma_end.as_usize() - start.as_usize();
                let right = VmArea {
                    id: next_vma_id(),
                    range: start..vma_end,
                    prot: new_prot,
                    object: VmObject::new(right_size),
                    obj_offset: vma.obj_offset + right_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset + right_offset,
                    file_size: vma.file_size.saturating_sub(right_offset),
                };
                self.areas.insert(start, right);
            } else {
                // Partial overlap at start: split at `end`
                let left_size = end.as_usize() - vma_start.as_usize();
                let left = VmArea {
                    id: next_vma_id(),
                    range: vma_start..end,
                    prot: new_prot,
                    object: VmObject::new(left_size),
                    obj_offset: vma.obj_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset,
                    file_size: core::cmp::min(vma.file_size, left_size),
                };
                self.areas.insert(vma_start, left);

                let right_offset = end.as_usize() - vma_start.as_usize();
                let right_size = vma_end.as_usize() - end.as_usize();
                let right = VmArea {
                    id: next_vma_id(),
                    range: end..vma_end,
                    prot: vma.prot,
                    object: VmObject::new(right_size),
                    obj_offset: vma.obj_offset + right_offset,
                    vma_type: vma.vma_type,
                    flags: vma.flags,
                    vnode: vma.vnode.clone(),
                    file_offset: vma.file_offset + right_offset,
                    file_size: vma.file_size.saturating_sub(right_offset),
                };
                self.areas.insert(end, right);
            }
        }
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
    use super::super::vm_object::VmObject;
    use super::*;

    fn make_vma(start: usize, end: usize) -> VmArea {
        let obj = VmObject::new((end - start) as usize);
        VmArea::new(
            VirtAddr::new(start)..VirtAddr::new(end),
            MapPerm::R | MapPerm::W,
            obj,
            VirtPageNum(0),
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
            w.insert_page(
                VirtPageNum(0),
                super::super::vm_object::OwnedPage::new_test(hal_common::PhysAddr::new(0xA000)),
            );
        }
        let vma = VmArea::new(
            VirtAddr::new(0x1000)..VirtAddr::new(0x2000),
            MapPerm::R | MapPerm::W,
            obj,
            VirtPageNum(0),
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
            w.insert_page(
                VirtPageNum(0),
                super::super::vm_object::OwnedPage::new_test(hal_common::PhysAddr::new(
                    0xDEAD_0000,
                )),
            );
            w.insert_page(
                VirtPageNum(0),
                super::super::vm_object::OwnedPage::new_test(hal_common::PhysAddr::new(
                    0xBEEF_0000,
                )),
            );
        }
        let vma = VmArea::new(
            VirtAddr::new(0x1000)..VirtAddr::new(0x3000),
            MapPerm::R | MapPerm::W,
            obj,
            VirtPageNum(0),
            VmAreaType::Anonymous,
        );
        parent_map.insert(vma).unwrap();

        let child_map = parent_map.fork();
        let child_vma = child_map.find_area(VirtAddr::new(0x1000)).unwrap();
        let child_obj = child_vma.object.read();
        // Shadow depth should be 1 (child shadow -> parent object)
        assert_eq!(child_obj.shadow_depth(), 1);
        // Parent's pages visible through shadow chain
        assert_eq!(
            child_obj.lookup_page(VirtPageNum(0)).unwrap(),
            hal_common::PhysAddr::new(0xDEAD_0000)
        );
        assert_eq!(
            child_obj.lookup_page(VirtPageNum(1)).unwrap(),
            hal_common::PhysAddr::new(0xBEEF_0000)
        );
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
            VirtPageNum(0),
            VmAreaType::Anonymous,
        );
        assert!(map.insert(vma).is_err());
    }
}
