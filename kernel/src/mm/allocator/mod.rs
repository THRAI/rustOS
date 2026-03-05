//! Memory allocator subsystem.
//!
//! - `buddy`: Buddy system frame allocator with split/coalesce
//! - `magazine`: Per-CPU magazine cache for lock-free order-0 fast path
//! - `frame_allocator`: Dual API (async + sync) frame allocation
//! - `types`: Strongly-typed frame wrappers (`TypedFrame`, `UserAnon`, `FileCache`)

pub mod buddy;
pub mod magazine;
pub mod types; // <-- Added types module

#[cfg(not(test))]
pub mod frame_allocator;

#[cfg(not(test))]
pub use frame_allocator::*;
pub use types::*; // <-- Export types

/// Test-only stub: alloc_raw_frame_sync backed by a simple atomic counter.
#[cfg(all(test, feature = "qemu-test"))]
pub fn alloc_raw_frame_sync(_role: PageRole) -> Option<hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0x8000_0000);
    let addr = NEXT_FRAME.fetch_add(hal_common::PAGE_SIZE, Ordering::Relaxed);
    Some(hal_common::PhysAddr::new(addr))
}

#[cfg(all(test, feature = "qemu-test"))]
pub fn frame_free(_frame: &'static mut crate::mm::vm::page::VmPage) {}

#[cfg(all(test, feature = "qemu-test"))]
pub fn frame_free_usage(_frame: &'static mut crate::mm::vm::page::VmPage) {}

#[cfg(all(test, feature = "qemu-test"))]
pub fn frame_alloc_usage(
    frame: &'static mut crate::mm::vm::page::VmPage,
) -> &'static mut crate::mm::vm::page::VmPage {
    crate::mm::allocator::types::get_frame_meta(frame.phys_addr).unwrap()
}

#[cfg(all(test, feature = "qemu-test"))]
macro_rules! define_test_alloc_wrapper {
    ($name:ident, $name_sync:ident, $role:ident) => {
        pub async fn $name() -> Option<&'static mut crate::mm::vm::page::VmPage> {
            alloc_raw_frame_sync(PageRole::$role)
                .map(|phys| crate::mm::allocator::types::get_frame_meta(phys).unwrap())
        }
        pub fn $name_sync() -> Option<&'static mut crate::mm::vm::page::VmPage> {
            alloc_raw_frame_sync(PageRole::$role)
                .map(|phys| crate::mm::allocator::types::get_frame_meta(phys).unwrap())
        }
    };
}

#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_anon, alloc_anon_sync, UserAnon);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_pte_l0, alloc_pte_l0_sync, PteL0);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_pte_l1, alloc_pte_l1_sync, PteL1);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_pte_l2, alloc_pte_l2_sync, PteL2);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_kstack, alloc_kstack_sync, KernelStack);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_ustack, alloc_ustack_sync, UserStack);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_file_cache, alloc_file_cache_sync, FileCache);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_user_exec, alloc_user_exec_sync, UserExec);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(alloc_pipe, alloc_pipe_sync, PipeBuffer);
#[cfg(all(test, feature = "qemu-test"))]
define_test_alloc_wrapper!(
    alloc_sig_trampoline,
    alloc_sig_trampoline_sync,
    SigTrampoline
);

#[cfg(all(test, feature = "qemu-test"))]
pub fn free_raw_frame(_addr: hal_common::PhysAddr) {}

#[cfg(all(test, feature = "qemu-test"))]
pub fn frame_alloc_contiguous(count: usize) -> Option<hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME_CONTIG: AtomicUsize = AtomicUsize::new(0x9000_0000);
    let addr = NEXT_FRAME_CONTIG.fetch_add(hal_common::PAGE_SIZE * (1 << count), Ordering::Relaxed);
    Some(hal_common::PhysAddr::new(addr))
}

#[cfg(all(test, feature = "qemu-test"))]
pub fn frame_free_contiguous(_addr: hal_common::PhysAddr, _order: usize) {}

#[cfg(all(test, feature = "qemu-test"))]
pub fn init_frame_allocator(_start: hal_common::PhysAddr, _end: hal_common::PhysAddr) {}
