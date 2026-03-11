//! Memory allocator subsystem — internal to the VM layer.
//!
//! **API boundary:** Process-level code (exec, fork) and filesystem code should
//! NOT call these functions directly. Instead, use VmObject methods:
//! - `VmObject::fetch_page_async()` — fetch a page through the pager
//! - `VmObject::fault_allocate_anon()` — allocate a zeroed anonymous page
//! - `VmObject::new_vnode_region()` — create a file-backed VmObject
//! - `VmObject::new_file()` — create a VmObject for a whole vnode
//!
//! Direct allocator use is reserved for VM internals (fault handlers, pmap),
//! drivers (DMA buffers), and boot-time infrastructure.
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

pub use buddy::BuddyAllocator;
#[cfg(not(test))]
pub use frame_allocator::*;
pub use magazine::Magazine;
pub use types::*; // <-- Export types
pub(crate) use types::{FRAME_META, FRAME_META_LEN};

/// Test-only stub: alloc_raw_frame_sync backed by a simple atomic counter.
#[cfg(test)]
pub fn alloc_raw_frame_sync(_role: PageRole) -> Option<crate::hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0x8000_0000);
    let addr = NEXT_FRAME.fetch_add(crate::hal_common::PAGE_SIZE, Ordering::Relaxed);
    Some(crate::hal_common::PhysAddr::new(addr))
}

#[cfg(test)]
pub fn frame_free(_frame: &'static mut crate::mm::VmPage) {}

#[cfg(test)]
pub fn frame_free_usage(_frame: &'static mut crate::mm::VmPage) {}

#[cfg(test)]
pub fn frame_alloc_usage(frame: &'static mut crate::mm::VmPage) -> &'static mut crate::mm::VmPage {
    get_frame_meta(frame.phys_addr).unwrap()
}

#[cfg(test)]
macro_rules! define_test_alloc_wrapper {
    ($name:ident, $name_sync:ident, $role:ident) => {
        pub async fn $name() -> Option<&'static mut crate::mm::VmPage> {
            alloc_raw_frame_sync(PageRole::$role).map(|phys| get_frame_meta(phys).unwrap())
        }
        pub fn $name_sync() -> Option<&'static mut crate::mm::VmPage> {
            alloc_raw_frame_sync(PageRole::$role).map(|phys| get_frame_meta(phys).unwrap())
        }
    };
}

#[cfg(test)]
define_test_alloc_wrapper!(alloc_anon, alloc_anon_sync, UserAnon);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pte_l0, alloc_pte_l0_sync, PteL0);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pte_l1, alloc_pte_l1_sync, PteL1);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pte_l2, alloc_pte_l2_sync, PteL2);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_kstack, alloc_kstack_sync, KernelStack);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_ustack, alloc_ustack_sync, UserStack);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_file_cache, alloc_file_cache_sync, FileCache);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_user_exec, alloc_user_exec_sync, UserExec);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pipe, alloc_pipe_sync, PipeBuffer);
#[cfg(test)]
define_test_alloc_wrapper!(
    alloc_sig_trampoline,
    alloc_sig_trampoline_sync,
    SigTrampoline
);

#[cfg(test)]
pub fn free_raw_frame(_addr: crate::hal_common::PhysAddr) {}

#[cfg(test)]
pub fn frame_alloc_contiguous(count: usize) -> Option<crate::hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME_CONTIG: AtomicUsize = AtomicUsize::new(0x9000_0000);
    let addr = NEXT_FRAME_CONTIG.fetch_add(
        crate::hal_common::PAGE_SIZE * (1 << count),
        Ordering::Relaxed,
    );
    Some(crate::hal_common::PhysAddr::new(addr))
}

#[cfg(test)]
pub fn frame_free_contiguous(_addr: crate::hal_common::PhysAddr, _order: usize) {}

#[cfg(test)]
pub fn init_frame_allocator(
    _start: crate::hal_common::PhysAddr,
    _end: crate::hal_common::PhysAddr,
) {
}
