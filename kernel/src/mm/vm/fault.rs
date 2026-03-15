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

use super::super::pmap::{self, Pmap};
use crate::{
    hal_common::{Errno, PhysAddr, VirtAddr, PAGE_SIZE},
    mm::vm::{BackingStore, MapPerm, VObjIndex, VmMap, VmMapEntry},
};

/// Result of a synchronous page fault resolution attempt.
#[derive(Debug)]
pub enum FaultResult {
    /// Fault fully resolved; return to user.
    Resolved,
    /// File-backed page needs async I/O (Phase 3).
    NeedsAsyncIO,
    /// Fault could not be resolved.
    Error(Errno),
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
            return FaultResult::Error(kerr!(
                vm,
                debug,
                Errno::Efault,
                "fault NOT_MAPPED va={:#x}",
                fault_va.0
            ));
        },
        Some(vma) => vma,
    };

    crate::klog!(
        vm,
        debug,
        "fault ENTER va={:#x} aligned={:#x} prot={:?} range=[{:#x}..{:#x})",
        fault_va.0,
        fault_va_aligned.0,
        vma.protection,
        vma.start(),
        vma.end()
    );

    // 2. Check permissions.
    if !access_type.permitted_by(vma.protection) {
        return FaultResult::Error(kerr!(
            vm,
            debug,
            Errno::Efault,
            "fault PERM_DENIED va={:#x} prot={:?} w={} r={} x={}",
            fault_va.0,
            vma.protection,
            access_type.write,
            access_type.read,
            access_type.execute
        ));
    }

    // 3. Compute object offset.
    let (obj, obj_page_offset) = match &vma.store {
        BackingStore::Object { object, offset } => {
            let offset_bytes = offset + ((fault_va_aligned.0 as u64) - vma.start());
            (
                object.clone(),
                VObjIndex::from_bytes_floor(offset_bytes as usize),
            )
        },
        BackingStore::SubMap { .. } | BackingStore::Guard => {
            return FaultResult::Error(kerr!(
                vm,
                debug,
                Errno::Efault,
                "fault INVALID_BACKING va={:#x} store=SubMap|Guard",
                fault_va_aligned.0
            ));
        },
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
    obj: Arc<crate::hal_common::LeveledRwLock<crate::mm::vm::VmObject, 3>>,
    obj_page_offset: VObjIndex,
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
        },
        Some(_old_phys) if access_type.write && vma.protection.contains(MapPerm::W) => {
            handle_cow_fault(vma, obj, obj_page_offset, fault_va_aligned, _old_phys, pmap)
        },
        Some(phys) => {
            if pmap::pmap_enter(pmap, fault_va_aligned, phys, vma.protection, false).is_err() {
                return FaultResult::Error(kerr!(
                    vm,
                    error,
                    Errno::Enomem,
                    "fault map FAILED va={:#x} pa={:#x} perm={:?}",
                    fault_va_aligned.0,
                    phys.as_usize(),
                    vma.protection
                ));
            }
            if pmap::pmap_extract(pmap, fault_va_aligned).is_none() {
                return FaultResult::Error(kerr!(
                    vm,
                    error,
                    Errno::Efault,
                    "fault map VERIFY_FAILED va={:#x} (pte missing right after enter)",
                    fault_va_aligned.0
                ));
            }
            FaultResult::Resolved
        },
    }
}

/// Handle an anonymous page fault: allocate a zeroed frame, insert into
/// the topmost VmObject, and map it.
fn handle_anonymous_fault(
    vma: &VmMapEntry,
    obj: Arc<crate::hal_common::LeveledRwLock<crate::mm::vm::VmObject, 3>>,
    obj_page_offset: VObjIndex,
    fault_va_aligned: VirtAddr,
    pmap: &mut Pmap,
) -> FaultResult {
    // Delegate allocation and insertion to the VmObject.
    let new_frame_phys = {
        let mut obj_write = obj.write();
        match obj_write.fault_allocate_anon(obj_page_offset) {
            Ok(phys) => phys,
            Err(_) => {
                return FaultResult::Error(kerr!(
                    vm,
                    error,
                    Errno::Enomem,
                    "anon fault OOM: alloc failed va={:#x}",
                    fault_va_aligned.0
                ))
            },
        }
    };

    // Map the page in the hardware page table.
    if pmap::pmap_enter(
        pmap,
        fault_va_aligned,
        new_frame_phys,
        vma.protection,
        false,
    )
    .is_err()
    {
        return FaultResult::Error(kerr!(
            vm,
            error,
            Errno::Enomem,
            "anon fault map FAILED va={:#x} pa={:#x}",
            fault_va_aligned.0,
            new_frame_phys.as_usize()
        ));
    }
    if pmap::pmap_extract(pmap, fault_va_aligned).is_none() {
        return FaultResult::Error(kerr!(
            vm,
            error,
            Errno::Efault,
            "anon fault VERIFY_FAILED va={:#x}",
            fault_va_aligned.0
        ));
    }

    FaultResult::Resolved
}

/// Handle a COW fault: copy the page to a new frame in the topmost
/// VmObject and remap with write permission.
fn handle_cow_fault(
    vma: &VmMapEntry,
    obj: Arc<crate::hal_common::LeveledRwLock<crate::mm::vm::VmObject, 3>>,
    obj_page_offset: VObjIndex,
    fault_va_aligned: VirtAddr,
    old_phys: PhysAddr,
    pmap: &mut Pmap,
) -> FaultResult {
    // Fast path: page is in THIS object (not a backing ancestor) AND no
    // child shadow exists. Safe to just flip the PTE to writable — no other
    // process can reach this page through the shadow chain.
    //
    // VmMap read lock is held by our caller (resolve_user_fault), which
    // excludes fork (fork takes VmMap write lock). This guarantees
    // shadow_count cannot increase between our check and pmap_protect.
    let can_promote = {
        let obj_read = obj.read();
        obj_read.has_page(obj_page_offset) && obj_read.shadow_count() == 0
    };
    if can_promote {
        pmap::pmap_protect(
            pmap,
            fault_va_aligned,
            VirtAddr(fault_va_aligned.0 + PAGE_SIZE),
            vma.protection,
        );
        return FaultResult::Resolved;
    }

    // Optimization: if our backing object is no longer shared by any sibling
    // shadow, collapse the chain and retry. This preserves COW semantics while
    // shortening future lookups.
    {
        let mut obj_write = obj.write();
        if let Some(backing) = obj_write.backing_object() {
            if backing.read().shadow_count() == 1 {
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
    }

    // Shared: need to copy the page. Delegate allocation and insertion.
    let new_frame_phys = {
        let mut obj_write = obj.write();
        match obj_write.fault_cow(obj_page_offset, old_phys) {
            Ok(phys) => phys,
            Err(_) => {
                return FaultResult::Error(kerr!(
                    vm,
                    error,
                    Errno::Enomem,
                    "cow fault OOM: copy alloc failed va={:#x}",
                    fault_va_aligned.0
                ))
            },
        }
    };

    if pmap::pmap_enter(
        pmap,
        fault_va_aligned,
        new_frame_phys,
        vma.protection,
        false,
    )
    .is_err()
    {
        return FaultResult::Error(kerr!(
            vm,
            error,
            Errno::Enomem,
            "cow fault map FAILED va={:#x} pa={:#x}",
            fault_va_aligned.0,
            new_frame_phys.as_usize()
        ));
    }
    if pmap::pmap_extract(pmap, fault_va_aligned).is_none() {
        return FaultResult::Error(kerr!(
            vm,
            error,
            Errno::Efault,
            "cow fault VERIFY_FAILED va={:#x}",
            fault_va_aligned.0
        ));
    }

    FaultResult::Resolved
}
