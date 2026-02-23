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
use spin::RwLock;

use hal_common::{PhysAddr, VirtAddr, PAGE_SIZE};

use super::vm_map::{MapPerm, VmArea, VmAreaType, VmMap};
use super::vm_object::{OwnedPage, VmObject};
use crate::mm::allocator::frame_alloc_sync;

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
}

/// Access type that caused the page fault.
#[derive(Debug, Clone, Copy)]
pub struct PageFaultAccessType {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl PageFaultAccessType {
    pub const READ: Self = Self { read: true, write: false, execute: false };
    pub const WRITE: Self = Self { read: false, write: true, execute: false };
    pub const EXECUTE: Self = Self { read: false, write: false, execute: true };

    /// Check if the access is permitted by the given VMA protection.
    fn permitted_by(&self, prot: MapPerm) -> bool {
        if self.read && !prot.contains(MapPerm::R) { return false; }
        if self.write && !prot.contains(MapPerm::W) { return false; }
        if self.execute && !prot.contains(MapPerm::X) { return false; }
        true
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
    fault_va: VirtAddr,
    access_type: PageFaultAccessType,
) -> FaultResult {
    // Page-align the faulting address.
    let fault_va_aligned = VirtAddr(fault_va.0 & !(PAGE_SIZE - 1));

    // 1. Find the VMA containing the faulting address.
    let vma = match vm_map.find_area(fault_va) {
        Some(vma) => vma,
        None => return FaultResult::Error(FaultError::NotMapped),
    };

    // 2. Check permissions.
    if !access_type.permitted_by(vma.prot) {
        // Special case: write to a readable VMA might be COW.
        // COW is detected below when we find a read-only mapping
        // in a writable VMA. If the VMA itself doesn't allow write,
        // it's truly invalid.
        if !(access_type.write && vma.prot.contains(MapPerm::R)) {
            return FaultResult::Error(FaultError::InvalidAccess);
        }
    }

    // 3. Compute object offset.
    let offset = ((fault_va_aligned.0 - vma.range.start.0) / PAGE_SIZE) as u64
        + vma.obj_offset;

    // 4. Classify and handle the fault.
    classify_and_handle(vma, offset, fault_va_aligned, access_type)
}

/// Classify the fault and handle it.
fn classify_and_handle(
    vma: &VmArea,
    offset: u64,
    fault_va_aligned: VirtAddr,
    access_type: PageFaultAccessType,
) -> FaultResult {
    // Check if the VMA is file-backed (Cached type).
    if vma.vma_type == VmAreaType::FileBacked || vma.vma_type == VmAreaType::Device {
        // File-backed faults need async I/O -- deferred to Phase 3.
        return FaultResult::NeedsAsyncIO;
    }

    // Look up the page in the shadow chain.
    let existing_page = {
        let obj = vma.object.read();
        obj.lookup_page(offset)
    };

    match existing_page {
        None => {
            // (a) Anonymous fault: page not in any VmObject in chain.
            handle_anonymous_fault(vma, offset, fault_va_aligned)
        }
        Some(_old_phys) if access_type.write && vma.prot.contains(MapPerm::W) => {
            // (b) COW fault: write to read-only page in a writable VMA.
            handle_cow_fault(vma, offset, fault_va_aligned, _old_phys)
        }
        Some(_) => {
            // Page exists and access is not a write-to-COW.
            // This shouldn't normally happen (page is mapped but we faulted).
            // Could be a race or a permission issue already checked above.
            FaultResult::Error(FaultError::InvalidAccess)
        }
    }
}

/// Handle an anonymous page fault: allocate a zeroed frame, insert into
/// the topmost VmObject, and map it.
fn handle_anonymous_fault(
    vma: &VmArea,
    offset: u64,
    _fault_va_aligned: VirtAddr,
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

    // TODO: pmap_enter(fault_va_aligned, new_frame, vma.prot)
    // The pmap_enter call will be wired when the pmap module is
    // integrated into the kernel crate. For now, the page is resident
    // in the VmObject and ready for mapping.

    FaultResult::Resolved
}

/// Handle a COW fault: copy the page to a new frame in the topmost
/// VmObject and remap with write permission.
fn handle_cow_fault(
    vma: &VmArea,
    offset: u64,
    _fault_va_aligned: VirtAddr,
    old_phys: PhysAddr,
) -> FaultResult {
    // Check if the topmost VmObject is the sole owner (refcount == 1).
    // If so, we can just upgrade permissions without copying.
    let refcount = Arc::strong_count(&vma.object);
    if refcount == 1 {
        // Sole owner: just upgrade permissions, no copy needed.
        // TODO: pmap_protect(fault_va_aligned, vma.prot) to add Write
        return FaultResult::Resolved;
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

    // TODO: pmap_enter(fault_va_aligned, new_frame, vma.prot | Write)
    // Will be wired when pmap is integrated into the kernel crate.

    FaultResult::Resolved
}

/// Zero a physical page via the kernel direct-map.
///
/// Uses the same KERNEL_ADDR_SPACE offset pattern as the pmap layer.
#[inline]
fn zero_page(phys: PhysAddr) {
    // TODO: use Constant::KERNEL_ADDR_SPACE.start for the direct-map offset
    // once hal-common exposes it. For now, use the raw address as a pointer
    // (valid in identity-mapped or direct-mapped kernel contexts).
    let ptr = phys.as_usize() as *mut u8;
    unsafe {
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
    }
}

/// Copy PAGE_SIZE bytes from one physical page to another.
#[inline]
fn copy_page(src: PhysAddr, dst: PhysAddr) {
    let src_ptr = src.as_usize() as *const u8;
    let dst_ptr = dst.as_usize() as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, PAGE_SIZE);
    }
}
