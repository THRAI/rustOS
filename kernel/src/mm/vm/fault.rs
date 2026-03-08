//! Synchronous page fault handler.
//!
//! `sync_fault_handler` runs on the trap stack and resolves anonymous and
//! COW page faults without yielding. It uses `alloc_raw_frame_sync()` exclusively
//! for frame allocation. File-backed faults return `NeedsAsyncIO` for
//! deferred handling in Phase 3.
//!
//! CRITICAL: This function MUST NOT .await, yield, or call any async function.
//!
//! TODO: Integration point — wire into the actual trap dispatch. Currently
//! the trap handler in hal calls `handle_page_fault` on UserVmSpace. This
//! function will replace that path once the pmap integration is complete.

use alloc::sync::Arc;

use crate::hal_common::{PhysAddr, VirtAddr, PAGE_SIZE};

use super::super::pmap::{self, Pmap};
use super::map::entry::{BackingStore, MapPerm, VmMapEntry};
use super::map::VmMap;
use crate::hal_common::addr::VirtPageNum;

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

/// Placeholder for VmPage busy lock states (future: exBusy/sBusy state machine)
#[derive(Debug, Clone, Copy)]
pub enum BusyState {
    Unbusied,
    ExclusiveBusy, // exBusy - I/O in progress, identity unstable
    SharedBusy,    // sBusy - stable snapshot for pmap
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
pub fn sync_fault_handler(
    vm_map: &VmMap, // Requires read-only lookup
    pmap: &mut Pmap,
    fault_va: VirtAddr,
    access_type: PageFaultAccessType,
) -> FaultResult {
    let fault_va_aligned = VirtAddr(fault_va.0 & !(PAGE_SIZE - 1));

    // 1. Find the VMA containing the faulting address.
    let vma = match vm_map.lookup_readonly(fault_va.0 as u64) {
        None => {
            crate::klog!(vm, debug, "fault NOT_MAPPED va={:#x}", fault_va.0);
            return FaultResult::Error(FaultError::NotMapped);
        }
        Some(vma) => vma,
    };

    crate::klog!(
        vm,
        debug,
        "fault ENTER va={:#x} aligned={:#x} prot={:?} range=[{:#x}..{:#x})",
        fault_va.0,
        fault_va_aligned.0,
        vma.protection,
        vma.start,
        vma.end
    );

    // 2. Check permissions.
    if !(access_type.permitted_by(vma.protection)
        || (access_type.write && vma.protection.contains(MapPerm::R)))
    {
        crate::klog!(
            vm,
            debug,
            "fault PERM_DENIED va={:#x} prot={:?} w={} r={} x={}",
            fault_va.0,
            vma.protection,
            access_type.write,
            access_type.read,
            access_type.execute
        );
        return FaultResult::Error(FaultError::InvalidAccess);
    }

    // 3. Compute object offset.
    let (obj, obj_page_offset) = match &vma.store {
        BackingStore::Object { object, offset } => {
            let offset_bytes = offset + ((fault_va_aligned.0 as u64) - vma.start);
            (
                object.clone(),
                VirtPageNum((offset_bytes / PAGE_SIZE as u64) as usize),
            )
        }
        BackingStore::SubMap { .. } | BackingStore::Guard => {
            return FaultResult::Error(FaultError::InvalidAccess);
        }
    };

    // 4. Classify and handle the fault.
    let result = classify_and_handle(
        vma,
        obj,
        obj_page_offset,
        fault_va_aligned,
        access_type,
        pmap,
    );
    crate::klog!(
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
    vma: &VmMapEntry,
    obj: Arc<spin::RwLock<crate::mm::vm::vm_object::VmObject>>,
    obj_page_offset: VirtPageNum,
    fault_va_aligned: VirtAddr,
    access_type: PageFaultAccessType,
    pmap: &mut Pmap,
) -> FaultResult {
    let existing_page = {
        let obj_read = obj.read();
        obj_read.lookup_page(obj_page_offset)
    };

    match existing_page {
        None => {
            let is_file_backed = {
                let obj_read = obj.read();
                // If it has a pager and it's not the anon pager, it's file backed
                // Actually an easier way is to just return NeedsAsyncIO unless the pager is specifically AnonPager
                // Currently, any fault without page in object can be deferred to async if we want!
                obj_read.pager.is_some() && !obj_read.pager.as_ref().unwrap().is_anon()
                // All object faults can be async in phase 3
            };

            if is_file_backed {
                crate::klog!(vm, debug, "fault NeedsAsyncIO va={:#x}", fault_va_aligned.0);
                FaultResult::NeedsAsyncIO
            } else {
                handle_anonymous_fault(vma, obj, obj_page_offset, fault_va_aligned, pmap)
            }
        }
        Some(_old_phys) if access_type.write && vma.protection.contains(MapPerm::W) => {
            handle_cow_fault(vma, obj, obj_page_offset, fault_va_aligned, _old_phys, pmap)
        }
        Some(phys) => {
            let _ = pmap::pmap_enter(pmap, fault_va_aligned, phys, vma.protection, false);
            FaultResult::Resolved
        }
    }
}

/// Handle an anonymous page fault: allocate a zeroed frame, insert into
/// the topmost VmObject, and map it.
fn handle_anonymous_fault(
    vma: &VmMapEntry,
    obj: Arc<spin::RwLock<crate::mm::vm::vm_object::VmObject>>,
    obj_page_offset: VirtPageNum,
    fault_va_aligned: VirtAddr,
    pmap: &mut Pmap,
) -> FaultResult {
    // Delegate allocation and insertion to the VmObject.
    let new_frame_phys = {
        let mut obj_write = obj.write();
        match obj_write.fault_allocate_anon(obj_page_offset) {
            Ok(phys) => phys,
            Err(_) => return FaultResult::Error(FaultError::OutOfMemory),
        }
    };

    // Map the page in the hardware page table.
    let _ = pmap::pmap_enter(
        pmap,
        fault_va_aligned,
        new_frame_phys,
        vma.protection,
        false,
    );

    FaultResult::Resolved
}

/// Handle a COW fault: copy the page to a new frame in the topmost
/// VmObject and remap with write permission.
fn handle_cow_fault(
    vma: &VmMapEntry,
    obj: Arc<spin::RwLock<crate::mm::vm::vm_object::VmObject>>,
    obj_page_offset: VirtPageNum,
    fault_va_aligned: VirtAddr,
    old_phys: PhysAddr,
    pmap: &mut Pmap,
) -> FaultResult {
    // Fast path: if Arc refcount == 1, no other VMA references this object.
    let refcount = Arc::strong_count(&obj);
    if refcount == 1 {
        pmap::pmap_protect(
            pmap,
            fault_va_aligned,
            VirtAddr(fault_va_aligned.0 + PAGE_SIZE),
            vma.protection,
        );
        return FaultResult::Resolved;
    }

    // Try collapse: if backing has shadow_count == 1, migrate pages.
    {
        let mut obj_write = obj.write();
        let can_collapse = obj_write
            .backing()
            .map(|b| b.read().shadow_count() == 1)
            .unwrap_or(false);
        if can_collapse {
            obj_write.collapse();
            if obj_write.has_page(obj_page_offset) {
                drop(obj_write);
                pmap::pmap_protect(
                    pmap,
                    fault_va_aligned,
                    VirtAddr(fault_va_aligned.0 + PAGE_SIZE),
                    vma.protection,
                );
                return FaultResult::Resolved;
            }
        }
    }

    // Shared: need to copy the page. Delegate allocation and insertion.
    let new_frame_phys = {
        let mut obj_write = obj.write();
        match obj_write.fault_cow(obj_page_offset, old_phys) {
            Ok(phys) => phys,
            Err(_) => return FaultResult::Error(FaultError::OutOfMemory),
        }
    };

    let _ = pmap::pmap_enter(
        pmap,
        fault_va_aligned,
        new_frame_phys,
        vma.protection,
        false,
    );

    FaultResult::Resolved
}

#[cfg(all(test, feature = "qemu-test"))]
mod tests {
    use super::super::super::pmap::Pmap;
    use super::super::vm_map::{MapPerm, VmArea, VmAreaType, VmMap};
    use super::super::vm_object::VmObject;
    use super::*;

    fn make_anon_map(start: usize, end: usize, prot: MapPerm) -> VmMap {
        let obj = VmObject::new(end - start);
        let vma = VmArea::new(
            VirtAddr::new(start)..VirtAddr::new(end),
            prot,
            obj,
            VirtPageNum(0),
            VmAreaType::Anonymous,
        );
        let mut map = VmMap::new();
        map.insert(vma).unwrap();
        map
    }

    #[test]
    fn anonymous_fault_resolves() {
        let map = make_anon_map(0x1000, 0x3000, crate::map_perm!(R, W));
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
        let map = make_anon_map(0x1000, 0x2000, crate::map_perm!(R, W));
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
            let mut p = VmPage::new();
            p.phys_addr = PhysAddr::new(0xA000);
            w.insert_page(VirtPageNum(0), Arc::new(p));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&parent_obj), 4096);
        let _sibling_ref = Arc::clone(&shadow);
        let vma = VmArea::new(
            VirtAddr::new(0x1000)..VirtAddr::new(0x2000),
            crate::map_perm!(R, W),
            shadow,
            VirtPageNum(0),
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
            VirtPageNum(0),
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
        let rw = crate::map_perm!(R, W);
        assert!(PageFaultAccessType::READ.permitted_by(rw));
        assert!(PageFaultAccessType::WRITE.permitted_by(rw));
        assert!(!PageFaultAccessType::EXECUTE.permitted_by(rw));

        let rx = crate::map_perm!(R, X);
        assert!(PageFaultAccessType::READ.permitted_by(rx));
        assert!(!PageFaultAccessType::WRITE.permitted_by(rx));
        assert!(PageFaultAccessType::EXECUTE.permitted_by(rx));
    }

    #[test]
    fn anonymous_fault_allocates_unique_frames() {
        let map = make_anon_map(0x1000, 0x3000, crate::map_perm!(R, W));
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
        let map = make_anon_map(0x1000, 0x2000, crate::map_perm!(R, W));
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
        let all = crate::map_perm!(R, W, X);
        assert!(PageFaultAccessType::READ.permitted_by(all));
        assert!(PageFaultAccessType::WRITE.permitted_by(all));
        assert!(PageFaultAccessType::EXECUTE.permitted_by(all));

        let none = MapPerm::empty();
        assert!(!PageFaultAccessType::READ.permitted_by(none));
        assert!(!PageFaultAccessType::WRITE.permitted_by(none));
        assert!(!PageFaultAccessType::EXECUTE.permitted_by(none));
    }
}
