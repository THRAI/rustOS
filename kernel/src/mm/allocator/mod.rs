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
#[cfg(test)]
pub fn alloc_raw_frame_sync(_role: PageRole) -> Option<hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME: AtomicUsize = AtomicUsize::new(0x8000_0000);
    let addr = NEXT_FRAME.fetch_add(hal_common::PAGE_SIZE, Ordering::Relaxed);
    Some(hal_common::PhysAddr::new(addr))
}

#[cfg(test)]
pub fn frame_free<R>(_frame: TypedFrame<R>) {}

#[cfg(test)]
pub fn frame_free_usage<R>(_frame: TypedFrame<R>) {}

#[cfg(test)]
pub fn frame_alloc_usage<R>(frame: &TypedFrame<R>) -> TypedFrame<R> {
    TypedFrame {
        phys: frame.phys(),
        _marker: core::marker::PhantomData,
    }
}

#[cfg(test)]
macro_rules! define_test_alloc_wrapper {
    ($name:ident, $name_sync:ident, $role:ident, $marker:ident) => {
        pub async fn $name() -> Option<TypedFrame<$marker>> {
            alloc_raw_frame_sync(PageRole::$role).map(|phys| TypedFrame {
                phys,
                _marker: core::marker::PhantomData,
            })
        }
        pub fn $name_sync() -> Option<TypedFrame<$marker>> {
            alloc_raw_frame_sync(PageRole::$role).map(|phys| TypedFrame {
                phys,
                _marker: core::marker::PhantomData,
            })
        }
    };
}

#[cfg(test)]
define_test_alloc_wrapper!(alloc_anon, alloc_anon_sync, UserAnon, UserAnon);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pte_l0, alloc_pte_l0_sync, PteL0, PteL0);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pte_l1, alloc_pte_l1_sync, PteL1, PteL1);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pte_l2, alloc_pte_l2_sync, PteL2, PteL2);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_kstack, alloc_kstack_sync, KernelStack, KernelStack);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_ustack, alloc_ustack_sync, UserStack, UserStack);
#[cfg(test)]
define_test_alloc_wrapper!(
    alloc_file_cache,
    alloc_file_cache_sync,
    FileCache,
    FileCache
);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_user_exec, alloc_user_exec_sync, UserExec, UserExec);
#[cfg(test)]
define_test_alloc_wrapper!(alloc_pipe, alloc_pipe_sync, PipeBuffer, PipeBuffer);
#[cfg(test)]
define_test_alloc_wrapper!(
    alloc_sig_trampoline,
    alloc_sig_trampoline_sync,
    SigTrampoline,
    SigTrampoline
);

#[cfg(test)]
pub fn free_raw_frame(_addr: hal_common::PhysAddr) {}

#[cfg(test)]
pub fn frame_alloc_contiguous(count: usize) -> Option<hal_common::PhysAddr> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_FRAME_CONTIG: AtomicUsize = AtomicUsize::new(0x9000_0000);
    let addr = NEXT_FRAME_CONTIG.fetch_add(hal_common::PAGE_SIZE * (1 << count), Ordering::Relaxed);
    Some(hal_common::PhysAddr::new(addr))
}

#[cfg(test)]
pub fn frame_free_contiguous(_addr: hal_common::PhysAddr, _order: usize) {}

#[cfg(test)]
pub fn init_frame_allocator(_start: hal_common::PhysAddr, _end: hal_common::PhysAddr) {}
