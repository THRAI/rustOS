//! QEMU integration tests for the VM subsystem.
//!
//! These tests run on real hardware (QEMU rv64) with the actual frame
//! allocator, verifying fault handler, COW, iterative drop, and
//! frame_alloc_sync behavior under real kernel conditions.
use crate::mm::vm::vm_map::MapPerm;
use alloc::sync::Arc;

use hal_common::addr::VirtPageNum;
use hal_common::{VirtAddr, PAGE_SIZE};

use super::super::allocator::{frame_alloc_sync, free_raw_frame};
use super::super::pmap;
use super::vm_map::{VmArea, VmAreaType, VmMap};
use super::vm_object::{OwnedPage, VmObject};

/// Helper: wrap a borrowed VmPage into an OwnedPage::Anonymous.
fn make_anon_page(page: &'static mut crate::mm::vm::page::VmPage) -> OwnedPage {
    OwnedPage::new_anonymous(page)
}

/// Test: anonymous page fault resolves to a new zeroed frame.
pub fn test_anonymous_page_fault() {
    let obj = VmObject::new(PAGE_SIZE);
    let vma = VmArea::new(
        VirtAddr::new(0x1000_0000)..VirtAddr::new(0x1000_0000 + PAGE_SIZE),
        crate::map_perm!(R, W),
        obj,
        VirtPageNum(0),
        VmAreaType::Anonymous,
    );
    let mut map = VmMap::new();
    map.insert(vma).unwrap();

    let mut test_pmap = pmap::pmap_create();
    let result = super::fault::sync_fault_handler(
        &map,
        &mut test_pmap,
        VirtAddr::new(0x1000_0000),
        super::fault::PageFaultAccessType::READ,
    );
    match result {
        super::fault::FaultResult::Resolved => {
            // Verify a page was inserted into the VmObject
            let vma = map.find_area(VirtAddr::new(0x1000_0000)).unwrap();
            let obj = vma.object.read();
            assert!(
                obj.lookup_page(VirtPageNum(0)).is_some(),
                "page not inserted after fault"
            );
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
    let parent_page = alloc_anon_sync().expect("OOM in COW test");
    // Zero the frame (identity-mapped in bare mode)
    unsafe {
        let ptr = parent_page.phys().as_usize() as *mut u8;
        core::ptr::write_bytes(ptr, 0x42, PAGE_SIZE);
    }

    let parent_obj = VmObject::new(PAGE_SIZE);
    {
        let mut w = parent_obj.write();
        w.insert_page(VirtPageNum(0), make_anon_page(parent_page));
    }

    // Create shadow (simulates fork)
    let shadow = VmObject::new_shadow(Arc::clone(&parent_obj), PAGE_SIZE);
    // Hold sibling ref to force refcount > 1 (triggers COW path)
    let _sibling = Arc::clone(&shadow);

    let vma = VmArea::new(
        VirtAddr::new(0x2000_0000)..VirtAddr::new(0x2000_0000 + PAGE_SIZE),
        crate::map_perm!(R, W),
        shadow,
        VirtPageNum(0),
        VmAreaType::Anonymous,
    );
    let mut map = VmMap::new();
    map.insert(vma).unwrap();

    let mut test_pmap = pmap::pmap_create();
    let result = super::fault::sync_fault_handler(
        &map,
        &mut test_pmap,
        VirtAddr::new(0x2000_0000),
        super::fault::PageFaultAccessType::WRITE,
    );
    match result {
        super::fault::FaultResult::Resolved => {
            let vma = map.find_area(VirtAddr::new(0x2000_0000)).unwrap();
            let obj = vma.object.read();
            let _new_phys = obj
                .lookup_page(VirtPageNum(0))
                .expect("COW page not inserted");
            // COW may either:
            // - copy to a new frame (shared backing, no collapse possible), or
            // - rename the page in-place via collapse (sole shadow, zero-copy)
            // Both are correct. Verify the page is accessible in the top-level object.
            if obj.has_page(VirtPageNum(0)) {
                crate::kprintln!("vm cow fault PASS");
            } else {
                crate::kprintln!("vm cow fault FAIL: page not in top-level object");
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
    let frame1 = alloc_anon_sync();
    match frame1 {
        Some(f1) => {
            let p1 = f1.phys();
            // Free and re-alloc to verify the round-trip
<<<<<<< HEAD
            free_raw_frame(p1);
            let frame2 = alloc_anon_sync();
            match frame2 {
                Some(f2) => {
                    free_raw_frame(f2.phys());
=======
            free_raw_frame(f1);
            let frame2 = frame_alloc_sync();
            match frame2 {
                Some(f2) => {
                    free_raw_frame(f2);
>>>>>>> cc5c4d1de86f18ff3dd42e08a6503eed633cc46d
                    crate::kprintln!("vm frame_alloc_sync PASS");
                }
                None => {
                    crate::kprintln!("vm frame_alloc_sync FAIL: second alloc returned None");
                }
            }
        }
        None => {
            crate::kprintln!("vm frame_alloc_sync FAIL: first alloc returned None");
        }
    }
}

/// Test: fork bomb stress -- rapid shadow chain fan-out and teardown.
///
/// Simulates a fork bomb: one root object with N children, each child
/// forks again creating grandchildren. Then tear down in various orders
/// to stress shadow_count, collapse, and iterative Drop under pressure.
///
/// Verifies:
/// - shadow_count stays consistent through mass fork/exit
/// - collapse works correctly when siblings die and sole-shadow is reached
/// - No panics, no memory corruption, no deadlocks
pub fn test_fork_bomb_stress() {
    use alloc::vec::Vec;

    const NUM_CHILDREN: usize = 32;
    const PAGES_PER_OBJ: usize = 4;

    // Root object with some pages (simulates a process heap).
    let root = VmObject::new(PAGES_PER_OBJ * PAGE_SIZE);
    for i in 0..PAGES_PER_OBJ {
        let page = alloc_anon_sync().expect("OOM in fork bomb root");
        unsafe {
            let ptr = page.phys().as_usize() as *mut u8;
            core::ptr::write_bytes(ptr, (i + 1) as u8, PAGE_SIZE);
        }
        let mut w = root.write();
        w.insert_page(VirtPageNum(i as usize), make_anon_page(page));
    }

    // Phase 1: Fork bomb -- create N children, each shadowing root.
    let mut children: Vec<Arc<spin::RwLock<VmObject>>> = Vec::new();
    for _ in 0..NUM_CHILDREN {
        children.push(VmObject::new_shadow(
            Arc::clone(&root),
            PAGES_PER_OBJ * PAGE_SIZE,
        ));
    }

    // Verify shadow_count on root.
    {
        let r = root.read();
        if r.shadow_count() != NUM_CHILDREN {
            crate::kprintln!(
                "vm fork bomb FAIL: root shadow_count {} != {}",
                r.shadow_count(),
                NUM_CHILDREN
            );
            return;
        }
    }

    // Phase 2: Each child forks a grandchild (deeper chain).
    let mut grandchildren: Vec<Arc<spin::RwLock<VmObject>>> = Vec::new();
    for child in children.iter() {
        grandchildren.push(VmObject::new_shadow(
            Arc::clone(child),
            PAGES_PER_OBJ * PAGE_SIZE,
        ));
    }

    // Phase 3: Kill all children (simulates parent exits, child inherits).
    drop(children);

    // Phase 4: Drop grandchildren one by one.
    for (_i, gc) in grandchildren.into_iter().enumerate() {
        // Before dropping, verify grandchild can still see root's pages.
        {
            let r = gc.read();
            for p in 0..PAGES_PER_OBJ {
                if r.lookup_page(VirtPageNum(p)).is_none() {
                    crate::kprintln!("vm fork bomb FAIL: gc[{}] can't see page {}", _i, p);
                    return;
                }
            }
        }
        drop(gc);
    }

    // Root should have shadow_count == 0 now.
    {
        let r = root.read();
        if r.shadow_count() != 0 {
            crate::kprintln!(
                "vm fork bomb FAIL: root shadow_count {} after full teardown",
                r.shadow_count()
            );
            return;
        }
    }

    // Phase 5: Verify root pages are still intact.
    {
        let r = root.read();
        for p in 0..PAGES_PER_OBJ {
            if r.lookup_page(VirtPageNum(p)).is_none() {
                crate::kprintln!("vm fork bomb FAIL: root page {} missing after teardown", p);
                return;
            }
        }
    }

    // Clean up: drop root object (frames freed via RAII).
    drop(root);

    crate::kprintln!("vm fork bomb PASS");
}
