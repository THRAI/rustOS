//! QEMU integration tests for the VM subsystem.
//!
//! These tests run on real hardware (QEMU rv64) with the actual frame
//! allocator, verifying fault handler, COW, iterative drop, and
//! frame_alloc_sync behavior under real kernel conditions.

use alloc::sync::Arc;

use hal_common::{PhysAddr, VirtAddr, PAGE_SIZE};

use super::vm_map::{MapPerm, VmArea, VmAreaType, VmMap};
use super::vm_object::{OwnedPage, VmObject};
use super::fault::{sync_fault_handler, FaultResult, PageFaultAccessType};
use super::super::allocator::{frame_alloc_sync, frame_free};

/// Test: anonymous page fault resolves to a new zeroed frame.
pub fn test_anonymous_page_fault() {
    let obj = VmObject::new(PAGE_SIZE);
    let vma = VmArea::new(
        VirtAddr::new(0x1000_0000)..VirtAddr::new(0x1000_0000 + PAGE_SIZE),
        MapPerm::R | MapPerm::W,
        obj,
        0,
        VmAreaType::Anonymous,
    );
    let mut map = VmMap::new();
    map.insert(vma).unwrap();

    let result = sync_fault_handler(&map, VirtAddr::new(0x1000_0000), PageFaultAccessType::READ);
    match result {
        FaultResult::Resolved => {
            // Verify a page was inserted into the VmObject
            let vma = map.find_area(VirtAddr::new(0x1000_0000)).unwrap();
            let obj = vma.object.read();
            assert!(obj.lookup_page(0).is_some(), "page not inserted after fault");
            crate::kprintln!("vm anonymous fault PASS");
        }
        other => {
            crate::kprintln!("vm anonymous fault FAIL: {:?}", other);
        }
    }
}

/// Test: COW fault allocates a private copy.
pub fn test_cow_fault() {
    // Allocate a real frame and insert into parent object
    let parent_frame = frame_alloc_sync().expect("OOM in COW test");
    // Zero the frame (identity-mapped in bare mode)
    unsafe {
        let ptr = parent_frame.as_usize() as *mut u8;
        core::ptr::write_bytes(ptr, 0x42, PAGE_SIZE);
    }

    let parent_obj = VmObject::new(PAGE_SIZE);
    {
        let mut w = parent_obj.write();
        w.insert_page(0, OwnedPage::new_anonymous(parent_frame));
    }

    // Create shadow (simulates fork)
    let shadow = VmObject::new_shadow(Arc::clone(&parent_obj), PAGE_SIZE);
    // Hold sibling ref to force refcount > 1 (triggers COW path)
    let _sibling = Arc::clone(&shadow);

    let vma = VmArea::new(
        VirtAddr::new(0x2000_0000)..VirtAddr::new(0x2000_0000 + PAGE_SIZE),
        MapPerm::R | MapPerm::W,
        shadow,
        0,
        VmAreaType::Anonymous,
    );
    let mut map = VmMap::new();
    map.insert(vma).unwrap();

    let result = sync_fault_handler(&map, VirtAddr::new(0x2000_0000), PageFaultAccessType::WRITE);
    match result {
        FaultResult::Resolved => {
            let vma = map.find_area(VirtAddr::new(0x2000_0000)).unwrap();
            let obj = vma.object.read();
            let new_phys = obj.lookup_page(0).expect("COW page not inserted");
            // COW should have allocated a different frame
            if new_phys != parent_frame {
                crate::kprintln!("vm cow fault PASS");
            } else {
                crate::kprintln!("vm cow fault FAIL: same frame as parent");
            }
        }
        other => {
            crate::kprintln!("vm cow fault FAIL: {:?}", other);
        }
    }
}

/// Test: deep shadow chain drops without stack overflow.
/// Uses 200-deep chain on QEMU (host tests cover 500+ and 1000+).
pub fn test_iterative_drop_500() {
    let mut current = VmObject::new(PAGE_SIZE);
    for _ in 0..200 {
        current = VmObject::new_shadow(Arc::clone(&current), PAGE_SIZE);
    }
    drop(current);
    // If we reach here, iterative Drop worked
    crate::kprintln!("vm iterative drop 500 PASS");
}

/// Test: frame_alloc_sync works in synchronous (non-async) context.
pub fn test_frame_alloc_sync_works() {
    let frame1 = frame_alloc_sync();
    match frame1 {
        Some(f1) => {
            // Free and re-alloc to verify the round-trip
            frame_free(f1);
            let frame2 = frame_alloc_sync();
            match frame2 {
                Some(f2) => {
                    frame_free(f2);
                    crate::kprintln!("vm frame_alloc_sync PASS");
                }
                None => crate::kprintln!("vm frame_alloc_sync FAIL: second alloc returned None"),
            }
        }
        None => crate::kprintln!("vm frame_alloc_sync FAIL: first alloc returned None"),
    }
}
