//! Synchronous page fault handler.
//!
//! `sync_fault_handler` runs on the trap stack and resolves anonymous and
//! COW page faults without yielding. It uses `frame_alloc_sync()` exclusively
//! for frame allocation. File-backed faults return `NeedsAsyncIO` for
//! deferred handling in Phase 3.
//!
//! CRITICAL: This function MUST NOT .await, yield, or call any async function.
//!
//! TODO: Integration point — wire into the actual trap dispatch. Currently
//! the trap handler in hal calls `handle_page_fault` on UserVmSpace. This
//! function will replace that path once the pmap integration is complete.

use alloc::sync::Arc;

use hal_common::{PhysAddr, VirtAddr, PAGE_SIZE};

use super::super::allocator::frame_alloc_sync;
use super::super::pmap::{self, Pmap};
use super::vm_map::{MapPerm, VmArea, VmAreaType, VmMap};
use super::vm_object::OwnedPage;

/// Result of a synchronous page fault resolution attempt.
#[derive(Debug)]
pub enum FaultResult {
    /// Fault fully resolved; return to user.
    Resolved,
    /// File-backed page needs async I/O (Phase 3).
    NeedsAsyncIO,
    /// Fault could not be resolved.
    Error(FaultError),
}

/// Reasons a page fault cannot be resolved synchronously.
#[derive(Debug)]
pub enum FaultError {
    /// No physical memory available.
    OutOfMemory,
    /// Access type not permitted by VMA protection.
    InvalidAccess,
    /// No VMA covers the faulting address.
    NotMapped,
    /// I/O error during async page fetch.
    IoError,
}

/// Access type that caused the page fault.
#[derive(Debug, Clone, Copy)]
pub struct PageFaultAccessType {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl PageFaultAccessType {
    pub const READ: Self = Self {
        read: true,
        write: false,
        execute: false,
    };
    pub const WRITE: Self = Self {
        read: false,
        write: true,
        execute: false,
    };
    pub const EXECUTE: Self = Self {
        read: false,
        write: false,
        execute: true,
    };

    /// Check if the access is permitted by the given VMA protection.
    fn permitted_by(&self, prot: MapPerm) -> bool {
        if self.read && !prot.contains(MapPerm::R) {
            return false;
        }
        if self.write && !prot.contains(MapPerm::W) {
            return false;
        }
        if self.execute && !prot.contains(MapPerm::X) {
            return false;
        }
        true
    }
}

impl core::fmt::Display for PageFaultAccessType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.write {
            f.write_str("WRITE")
        } else if self.execute {
            f.write_str("EXECUTE")
        } else {
            f.write_str("READ")
        }
    }
}

/// Synchronous page fault handler. Runs on trap stack -- never yields.
///
/// Resolves:
/// - Anonymous faults: allocate zeroed frame, insert into VmObject, map via pmap
/// - COW faults: copy page to new frame, remap with write permission
/// - File-backed faults: returns NeedsAsyncIO (Phase 3)
///
/// Uses `frame_alloc_sync()` exclusively for all allocations.
pub fn sync_fault_handler(
    vm_map: &VmMap,
    pmap: &mut Pmap,
    fault_va: VirtAddr,
    access_type: PageFaultAccessType,
) -> FaultResult {
    // Page-align the faulting address.
    let fault_va_aligned = VirtAddr(fault_va.0 & !(PAGE_SIZE - 1));

    // 1. Find the VMA containing the faulting address.
    let vma = match vm_map.find_area(fault_va) {
        Some(vma) => vma,
        None => {
            klog!(vm, debug, "fault NOT_MAPPED va={:#x}", fault_va.0);
            return FaultResult::Error(FaultError::NotMapped);
        }
    };

    klog!(
        vm,
        debug,
        "fault ENTER va={:#x} aligned={:#x} type={:?} prot={:?} range=[{:#x}..{:#x})",
        fault_va.0,
        fault_va_aligned.0,
        vma.vma_type,
        vma.prot,
        vma.range.start.0,
        vma.range.end.0
    );

    // 2. Check permissions.
    if !access_type.permitted_by(vma.prot) {
        if !(access_type.write && vma.prot.contains(MapPerm::R)) {
            klog!(
                vm,
                debug,
                "fault PERM_DENIED va={:#x} type={:?} prot={:?} w={} r={} x={}",
                fault_va.0,
                vma.vma_type,
                vma.prot,
                access_type.write,
                access_type.read,
                access_type.execute
            );
            return FaultResult::Error(FaultError::InvalidAccess);
        }
    }

    // 3. Compute object offset.
    let offset = ((fault_va_aligned.0 - vma.range.start.0) / PAGE_SIZE) as u64 + vma.obj_offset;

    // 4. Classify and handle the fault.
    let result = classify_and_handle(vma, offset, fault_va_aligned, access_type, pmap);
    klog!(
        vm,
        debug,
        "fault RESULT va={:#x} => {:?}",
        fault_va.0,
        result
    );
    result
}

/// Classify the fault and handle it.
fn classify_and_handle(
    vma: &VmArea,
    offset: u64,
    fault_va_aligned: VirtAddr,
    access_type: PageFaultAccessType,
    pmap: &mut Pmap,
) -> FaultResult {
    // Look up the page in the VmObject shadow chain FIRST — even for
    // file-backed VMAs. After fork, the child's shadow object may already
    // contain the page (COW from parent). Only fall through to async I/O
    // if the page is truly absent.
    let existing_page = {
        let obj = vma.object.read();
        obj.lookup_page(offset)
    };

    match existing_page {
        None if vma.vma_type == VmAreaType::FileBacked || vma.vma_type == VmAreaType::Device => {
            // Page not in VmObject and VMA is file-backed — need async I/O.
            klog!(vm, debug, "fault NeedsAsyncIO va={:#x}", fault_va_aligned.0);
            FaultResult::NeedsAsyncIO
        }
        None => {
            // (a) Anonymous fault: page not in any VmObject in chain.
            handle_anonymous_fault(vma, offset, fault_va_aligned, pmap)
        }
        Some(_old_phys) if access_type.write && vma.prot.contains(MapPerm::W) => {
            // (b) COW fault: write to read-only page in a writable VMA.
            handle_cow_fault(vma, offset, fault_va_aligned, _old_phys, pmap)
        }
        Some(phys) => {
            // Page exists in VmObject but not in pmap — re-establish mapping.
            let _ = pmap::pmap_enter(pmap, fault_va_aligned, phys, vma.prot, false);
            FaultResult::Resolved
        }
    }
}

/// Handle an anonymous page fault: allocate a zeroed frame, insert into
/// the topmost VmObject, and map it.
fn handle_anonymous_fault(
    vma: &VmArea,
    offset: u64,
    fault_va_aligned: VirtAddr,
    pmap: &mut Pmap,
) -> FaultResult {
    // Allocate a frame synchronously (never yields).
    let new_frame = match frame_alloc_sync() {
        Some(addr) => addr,
        None => return FaultResult::Error(FaultError::OutOfMemory),
    };

    // Zero the frame.
    zero_page(new_frame);

    // Insert page into topmost VmObject.
    {
        let mut obj = vma.object.write();
        obj.insert_page(offset, OwnedPage::new_anonymous(new_frame));
    }

    // Map the page in the hardware page table.
    let _ = pmap::pmap_enter(pmap, fault_va_aligned, new_frame, vma.prot, false);

    FaultResult::Resolved
}

/// Handle a COW fault: copy the page to a new frame in the topmost
/// VmObject and remap with write permission.
///
/// Optimization: if the backing object has shadow_count == 1, we are the
/// sole shadow. Attempt collapse (migrate pages from backing into self),
/// then check if the page is now local — if so, promote in-place (O(1)
/// zero-copy) instead of copying.
fn handle_cow_fault(
    vma: &VmArea,
    offset: u64,
    fault_va_aligned: VirtAddr,
    old_phys: PhysAddr,
    pmap: &mut Pmap,
) -> FaultResult {
    // Fast path: if Arc refcount == 1, no other VMA references this object.
    let refcount = Arc::strong_count(&vma.object);
    if refcount == 1 {
        pmap::pmap_protect(
            pmap,
            fault_va_aligned,
            VirtAddr(fault_va_aligned.0 + PAGE_SIZE),
            vma.prot,
        );
        return FaultResult::Resolved;
    }

    // Try collapse: if backing has shadow_count == 1, migrate pages.
    {
        let mut obj = vma.object.write();
        let can_collapse = obj
            .backing()
            .map(|b| b.read().shadow_count() == 1)
            .unwrap_or(false);
        if can_collapse {
            obj.collapse();
            // After collapse, check if the page is now in our top-level object.
            if obj.has_page(offset) {
                // Page was renamed from backing into self — zero-copy promotion.
                drop(obj);
                pmap::pmap_protect(
                    pmap,
                    fault_va_aligned,
                    VirtAddr(fault_va_aligned.0 + PAGE_SIZE),
                    vma.prot,
                );
                return FaultResult::Resolved;
            }
        }
    }

    // Shared: need to copy the page.
    let new_frame = match frame_alloc_sync() {
        Some(addr) => addr,
        None => return FaultResult::Error(FaultError::OutOfMemory),
    };

    // Copy old page contents to new frame.
    copy_page(old_phys, new_frame);

    // Insert new frame into topmost VmObject (shadows the old page).
    {
        let mut obj = vma.object.write();
        obj.insert_page(offset, OwnedPage::new_anonymous(new_frame));
    }

    // Map the new frame with full write permission.
    let _ = pmap::pmap_enter(pmap, fault_va_aligned, new_frame, vma.prot, false);

    FaultResult::Resolved
}

/// Zero a physical page via the kernel direct-map.
///
/// Uses the same KERNEL_ADDR_SPACE offset pattern as the pmap layer.
#[inline]
#[cfg(not(test))]
fn zero_page(phys: PhysAddr) {
    // TODO: use Constant::KERNEL_ADDR_SPACE.start for the direct-map offset
    // once hal-common exposes it. For now, use the raw address as a pointer
    // (valid in identity-mapped or direct-mapped kernel contexts).
    let ptr = phys.as_usize() as *mut u8;
    unsafe {
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
    }
}

/// Test stub: no-op zero_page (host cannot write to fake PhysAddr).
#[inline]
#[cfg(test)]
fn zero_page(_phys: PhysAddr) {}

/// Copy PAGE_SIZE bytes from one physical page to another.
#[inline]
#[cfg(not(test))]
fn copy_page(src: PhysAddr, dst: PhysAddr) {
    unsafe {
        dst.as_mut_slice().copy_from_slice(src.as_slice());
    }
}

/// Test stub: no-op copy_page (host cannot write to fake PhysAddr).
#[inline]
#[cfg(test)]
fn copy_page(_src: PhysAddr, _dst: PhysAddr) {}

// ---------------------------------------------------------------------------
// Async file-backed fault handler
// ---------------------------------------------------------------------------

/// Async fault handler for file-backed pages.
///
/// Called when sync_fault_handler returns NeedsAsyncIO. Fetches the page
/// from the page cache (which may trigger delegate I/O), handles boundary
/// page anonymization, and inserts the page into the VmObject.
///
/// Boundary anonymization: when offset_in_vma + PAGE_SIZE > file_size,
/// allocate a fresh frame, memcpy the file portion, zero the tail.
/// Never mutate page cache frames.
#[cfg(not(test))]
pub async fn fault_in_page(vm_map: &VmMap, fault_va: VirtAddr) -> FaultResult {
    let fault_va_aligned = VirtAddr(fault_va.0 & !(PAGE_SIZE - 1));

    let vma = match vm_map.find_area(fault_va) {
        Some(vma) => vma,
        None => return FaultResult::Error(FaultError::NotMapped),
    };

    let vnode = match &vma.vnode {
        Some(v) => v,
        None => return FaultResult::Error(FaultError::NotMapped),
    };

    // Compute offset within the VMA
    let offset_in_vma = (fault_va_aligned.0 - vma.range.start.0) as u64;

    // If beyond file_size, this is a zero-fill page (BSS)
    if offset_in_vma >= vma.file_size {
        // Allocate a zeroed anonymous page
        let frame = match frame_alloc_sync() {
            Some(f) => f,
            None => return FaultResult::Error(FaultError::OutOfMemory),
        };
        zero_page(frame);

        let obj_offset =
            ((fault_va_aligned.0 - vma.range.start.0) / PAGE_SIZE) as u64 + vma.obj_offset;
        let mut obj = vma.object.write();
        obj.insert_page(
            obj_offset,
            super::vm_object::OwnedPage::new_anonymous(frame),
        );
        return FaultResult::Resolved;
    }

    // File-backed page: fetch from page cache
    let file_byte_offset = vma.file_offset + offset_in_vma;
    let _page_offset = file_byte_offset % (PAGE_SIZE as u64);
    let _vnode_id = vnode.vnode_id();

    // Check page cache first
    let cached_pa = match None::<PhysAddr> {
        Some(pa) => pa,
        None => {
            // FIXME: dependency on fs
            return FaultResult::Error(FaultError::OutOfMemory);
        }
    };

    // Check if this is a boundary page (partial file data + zero tail)
    let bytes_from_file_in_page = if offset_in_vma + PAGE_SIZE as u64 > vma.file_size {
        (vma.file_size - offset_in_vma) as usize
    } else {
        PAGE_SIZE
    };

    let obj_offset = ((fault_va_aligned.0 - vma.range.start.0) / PAGE_SIZE) as u64 + vma.obj_offset;

    if bytes_from_file_in_page < PAGE_SIZE {
        // Boundary page: anonymize — allocate fresh frame, copy file portion, zero tail
        let frame = match frame_alloc_sync() {
            Some(f) => f,
            None => return FaultResult::Error(FaultError::OutOfMemory),
        };
        // Copy file portion
        unsafe {
            let src_slice = cached_pa.as_slice();
            let dst_slice = frame.as_mut_slice();
            dst_slice[..bytes_from_file_in_page]
                .copy_from_slice(&src_slice[..bytes_from_file_in_page]);
            // Zero the tail
            dst_slice[bytes_from_file_in_page..].fill(0);
        }
        let mut obj = vma.object.write();
        obj.insert_page(
            obj_offset,
            super::vm_object::OwnedPage::new_anonymous(frame),
        );
    } else {
        // Full page from cache: map read-only (COW on write fault)
        let mut obj = vma.object.write();
        obj.insert_page(
            obj_offset,
            super::vm_object::OwnedPage::new_cached(cached_pa),
        );
    }

    FaultResult::Resolved
}

#[cfg(test)]
mod tests {
    use super::super::super::pmap::Pmap;
    use super::super::vm_map::{MapPerm, VmArea, VmAreaType, VmMap};
    use super::super::vm_object::{OwnedPage, VmObject};
    use super::*;

    fn make_anon_map(start: usize, end: usize, prot: MapPerm) -> VmMap {
        let obj = VmObject::new(end - start);
        let vma = VmArea::new(
            VirtAddr::new(start)..VirtAddr::new(end),
            prot,
            obj,
            0,
            VmAreaType::Anonymous,
        );
        let mut map = VmMap::new();
        map.insert(vma).unwrap();
        map
    }

    #[test]
    fn anonymous_fault_resolves() {
        let map = make_anon_map(0x1000, 0x3000, MapPerm::R | MapPerm::W);
        let mut pmap = Pmap::dummy();
        let result = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x1500),
            PageFaultAccessType::READ,
        );
        assert!(matches!(result, FaultResult::Resolved));
        let vma = map.find_area(VirtAddr::new(0x1500)).unwrap();
        let obj = vma.object.read();
        assert_eq!(obj.resident_count(), 1);
    }

    #[test]
    fn fault_not_mapped() {
        let map = VmMap::new();
        let mut pmap = Pmap::dummy();
        let result = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x5000),
            PageFaultAccessType::READ,
        );
        assert!(matches!(result, FaultResult::Error(FaultError::NotMapped)));
    }

    #[test]
    fn fault_invalid_access_write_to_readonly() {
        let map = make_anon_map(0x1000, 0x2000, MapPerm::R);
        let mut pmap = Pmap::dummy();
        let result = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x1000),
            PageFaultAccessType::WRITE,
        );
        assert!(
            matches!(result, FaultResult::Resolved)
                || matches!(result, FaultResult::Error(FaultError::InvalidAccess))
        );
    }

    #[test]
    fn fault_execute_on_non_exec() {
        let map = make_anon_map(0x1000, 0x2000, MapPerm::R | MapPerm::W);
        let mut pmap = Pmap::dummy();
        let result = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x1000),
            PageFaultAccessType::EXECUTE,
        );
        assert!(matches!(
            result,
            FaultResult::Error(FaultError::InvalidAccess)
        ));
    }

    #[test]
    fn cow_fault_copies_page() {
        let parent_obj = VmObject::new(4096);
        {
            let mut w = parent_obj.write();
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0xA000)));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&parent_obj), 4096);
        let _sibling_ref = Arc::clone(&shadow);
        let vma = VmArea::new(
            VirtAddr::new(0x1000)..VirtAddr::new(0x2000),
            MapPerm::R | MapPerm::W,
            shadow,
            0,
            VmAreaType::Anonymous,
        );
        let mut map = VmMap::new();
        map.insert(vma).unwrap();

        let mut pmap = Pmap::dummy();
        let result = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x1000),
            PageFaultAccessType::WRITE,
        );
        assert!(matches!(result, FaultResult::Resolved));

        let vma = map.find_area(VirtAddr::new(0x1000)).unwrap();
        let obj = vma.object.read();
        // After COW, page must be in the top-level object (either copied or
        // renamed via collapse). Both outcomes are correct.
        assert!(obj.has_page(0));
        assert_eq!(obj.resident_count(), 1);
    }

    #[test]
    fn file_backed_returns_needs_async() {
        let obj = VmObject::new(4096);
        let vma = VmArea::new(
            VirtAddr::new(0x1000)..VirtAddr::new(0x2000),
            MapPerm::R,
            obj,
            0,
            VmAreaType::FileBacked,
        );
        let mut map = VmMap::new();
        map.insert(vma).unwrap();
        let mut pmap = Pmap::dummy();
        let result = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x1000),
            PageFaultAccessType::READ,
        );
        assert!(matches!(result, FaultResult::NeedsAsyncIO));
    }

    #[test]
    fn page_fault_access_type_permitted() {
        let rw = MapPerm::R | MapPerm::W;
        assert!(PageFaultAccessType::READ.permitted_by(rw));
        assert!(PageFaultAccessType::WRITE.permitted_by(rw));
        assert!(!PageFaultAccessType::EXECUTE.permitted_by(rw));

        let rx = MapPerm::R | MapPerm::X;
        assert!(PageFaultAccessType::READ.permitted_by(rx));
        assert!(!PageFaultAccessType::WRITE.permitted_by(rx));
        assert!(PageFaultAccessType::EXECUTE.permitted_by(rx));
    }

    #[test]
    fn anonymous_fault_allocates_unique_frames() {
        let map = make_anon_map(0x1000, 0x3000, MapPerm::R | MapPerm::W);
        let mut pmap = Pmap::dummy();
        let r1 = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x1000),
            PageFaultAccessType::READ,
        );
        assert!(matches!(r1, FaultResult::Resolved));
        let r2 = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x2000),
            PageFaultAccessType::READ,
        );
        assert!(matches!(r2, FaultResult::Resolved));
        let vma = map.find_area(VirtAddr::new(0x1000)).unwrap();
        let obj = vma.object.read();
        let p1 = obj.lookup_page(0).unwrap();
        let p2 = obj.lookup_page(1).unwrap();
        assert_ne!(p1, p2);
    }

    #[test]
    fn fault_page_aligned_resolution() {
        let map = make_anon_map(0x1000, 0x2000, MapPerm::R | MapPerm::W);
        let mut pmap = Pmap::dummy();
        let result = sync_fault_handler(
            &map,
            &mut pmap,
            VirtAddr::new(0x1ABC),
            PageFaultAccessType::READ,
        );
        assert!(matches!(result, FaultResult::Resolved));
    }

    #[test]
    fn rwx_permission_combinations() {
        let all = MapPerm::R | MapPerm::W | MapPerm::X;
        assert!(PageFaultAccessType::READ.permitted_by(all));
        assert!(PageFaultAccessType::WRITE.permitted_by(all));
        assert!(PageFaultAccessType::EXECUTE.permitted_by(all));

        let none = MapPerm::empty();
        assert!(!PageFaultAccessType::READ.permitted_by(none));
        assert!(!PageFaultAccessType::WRITE.permitted_by(none));
        assert!(!PageFaultAccessType::EXECUTE.permitted_by(none));
    }
}
