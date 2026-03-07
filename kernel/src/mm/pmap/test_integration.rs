//! QEMU integration tests for the pmap layer.
//!
//! Tier 2.a: Offline PT walk — create/enter/extract/remove without satp switch.
//! Tier 2.b: satp switch — identity-map kernel, map high VA, activate, read/write.

use crate::hal_common::{VirtAddr, PAGE_SIZE};

use super::super::allocator::{alloc_raw_frame_sync, PageRole};
use super::{pmap_activate, pmap_create, pmap_destroy, pmap_enter, pmap_extract, pmap_remove};

/// Tier 2.a: Verify pmap walk logic with real allocated frames.
/// No satp switch — purely tests the radix tree construction.
pub fn test_pmap_extract_only() {
    // 1. Create a fresh pmap (allocates root PT page)
    let mut pmap = pmap_create();

    // 2. Allocate a backing frame
    let frame = alloc_raw_frame_sync(PageRole::PteL1).expect("OOM in pmap extract test");

    // 3. Enter mapping: VA 0x1_0000_0000 → frame, R|W
    let va = VirtAddr::new(0x1_0000_0000);
    pmap_enter(&mut pmap, va, frame, crate::map_perm!(R, W), false).expect("pmap_enter failed");

    // 4. Extract and verify
    let extracted = pmap_extract(&pmap, va).expect("pmap_extract returned None");
    assert_eq!(extracted.as_usize(), frame.as_usize());

    // 5. Second mapping at different VA
    let va2 = VirtAddr::new(0x1_0000_1000);
    let frame2 = alloc_raw_frame_sync(PageRole::PteL1).expect("OOM");
    pmap_enter(&mut pmap, va2, frame2, crate::map_perm!(R, X), false).unwrap();
    assert_eq!(
        pmap_extract(&pmap, va2).unwrap().as_usize(),
        frame2.as_usize()
    );

    // 6. Unmapped VA returns None
    assert!(pmap_extract(&pmap, VirtAddr::new(0x1_0000_2000)).is_none());

    // 7. Remove and verify
    pmap_remove(&mut pmap, va, VirtAddr::new(va.as_usize() + PAGE_SIZE));
    assert!(pmap_extract(&pmap, va).is_none());

    // 8. Second mapping still intact
    assert_eq!(
        pmap_extract(&pmap, va2).unwrap().as_usize(),
        frame2.as_usize()
    );

    pmap_destroy(&mut pmap);
    crate::kprintln!("pmap extract-only PASS");
}

/// Tier 2.b: Activate pmap with identity-mapped kernel, write through a high VA.
///
/// The entire satp-active window is IRQ-locked to prevent preemption.
pub fn test_pmap_satp_switch() {
    let mut pmap = pmap_create();

    // pmap_create() now includes kernel identity mappings (1GB gigapage at
    // root entry 2) and MMIO megapage, so no manual kernel mapping needed.

    // 2. Map a high VA to a fresh frame
    let test_frame = alloc_raw_frame_sync(PageRole::PteL1).expect("OOM in satp test");
    let test_va = VirtAddr::new(0xDEAD_0000);
    pmap_enter(
        &mut pmap,
        test_va,
        test_frame,
        crate::map_perm!(R, W),
        false,
    )
    .unwrap();

    // === BEGIN IRQ-LOCKED WINDOW ===
    let saved = crate::hal_common::irq_lock::arch_irq::disable_and_save();

    // 3. Activate pmap (writes satp, sfence.vma)
    pmap_activate(&mut pmap);

    // 4. Write through the new virtual mapping
    unsafe {
        let ptr = test_va.as_usize() as *mut u64;
        ptr.write_volatile(0xCAFE_BABE);
        let readback = ptr.read_volatile();
        assert_eq!(readback, 0xCAFE_BABE);
    }

    // 5. Verify write landed at the physical frame (identity-mapped)
    unsafe {
        let phys_ptr = test_frame.as_usize() as *const u64;
        assert_eq!(phys_ptr.read_volatile(), 0xCAFE_BABE);
    }

    // 6. Deactivate: return to bare mode (satp=0)
    unsafe {
        core::arch::asm!("csrw satp, zero", "sfence.vma zero, zero",);
    }

    // === END IRQ-LOCKED WINDOW ===
    crate::hal_common::irq_lock::arch_irq::restore(saved);

    // 7. Cleanup
    pmap_destroy(&mut pmap);

    crate::kprintln!("pmap satp-switch PASS");
}
