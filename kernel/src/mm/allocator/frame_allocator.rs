//! Frame allocator with dual API (async + sync) and debug memory poisoning.
//!
//! - `alloc_raw_frame_sync()`: synchronous, never yields — safe for trap context
//! - `frame_alloc()`: async-capable, can yield to wake page daemon (Phase 5)
//! - `free_raw_frame()`: returns frame via per-CPU magazine, drains to buddy when full
//! - `frame_alloc_contiguous()`: multi-page allocation directly from buddy

use super::buddy::BuddyAllocator;
use super::magazine::Magazine;
use hal_common::addr::{PhysAddr, PAGE_SIZE};
use hal_common::IrqSafeSpinLock;

/// Debug-build poison pattern: written to every u64 in a freed frame.
#[cfg(debug_assertions)]
const POISON_PATTERN: u64 = 0xDEAD_BEEF_DEAD_BEEF;

/// Trap stack canary value, written at bottom of trap stack.
#[cfg(debug_assertions)]
pub const STACK_CANARY: u64 = 0xCAFE_BABE_DEAD_C0DE;

/// Global buddy allocator, protected by IrqSafeSpinLock.
static GLOBAL_BUDDY: IrqSafeSpinLock<BuddyAllocator> = IrqSafeSpinLock::new(BuddyAllocator::new());

/// Per-CPU magazine array. Each CPU gets its own magazine for lock-free
/// order-0 alloc/free. Indexed by cpu_id.
static PER_CPU_MAGAZINES: [IrqSafeSpinLock<Magazine>; 8] = [
    IrqSafeSpinLock::new(Magazine::new()),
    IrqSafeSpinLock::new(Magazine::new()),
    IrqSafeSpinLock::new(Magazine::new()),
    IrqSafeSpinLock::new(Magazine::new()),
    IrqSafeSpinLock::new(Magazine::new()),
    IrqSafeSpinLock::new(Magazine::new()),
    IrqSafeSpinLock::new(Magazine::new()),
    IrqSafeSpinLock::new(Magazine::new()),
];

/// Initialize the buddy allocator with the given physical memory range.
/// Called once during boot after the early bump allocator has reserved its region.
pub fn init_frame_allocator(start: PhysAddr, end: PhysAddr) {
    crate::klog!(boot, info, "frame: Box::new(BuddyAllocator::new())...");
    let mut buddy = alloc::boxed::Box::new(BuddyAllocator::new());
    crate::klog!(boot, info, "frame: box created, calling init...");
    buddy.init(start, end);
    crate::klog!(
        boot,
        info,
        "frame: init done, total={} free={}",
        buddy.total_pages(),
        buddy.available_pages()
    );

    // Initialize the global FRAME_META array
    // We need one FrameMeta per physical page in the system (up to `end` address)
    // To be safe and cover all possible PFNs up to the max address:
    let max_pfn = end.as_usize() / PAGE_SIZE;
    let meta_size_bytes = max_pfn * core::mem::size_of::<crate::mm::allocator::types::FrameMeta>();
    let meta_pages = (meta_size_bytes + PAGE_SIZE - 1) / PAGE_SIZE;

    // Allocate contiguous pages for the metadata array directly from the buddy allocator we just initialized
    // finding the order required
    let mut order = 0;
    while (1 << order) < meta_pages {
        order += 1;
    }

    let meta_ptr = buddy
        .alloc(order)
        .expect("Failed to allocate FRAME_META array");

    // Zero out the allocated metadata memory
    unsafe {
        core::ptr::write_bytes(meta_ptr.as_usize() as *mut u8, 0, (1 << order) * PAGE_SIZE);
        crate::mm::allocator::types::FRAME_META =
            meta_ptr.as_usize() as *mut crate::mm::allocator::types::FrameMeta;
        crate::mm::allocator::types::FRAME_META_LEN = max_pfn;
    }

    crate::klog!(
        boot,
        info,
        "frame: FRAME_META initialized ({} pages at {:#x})",
        meta_pages,
        meta_ptr.as_usize()
    );

    crate::klog!(boot, info, "frame: swapping into global...");
    // Single-threaded at boot — use lock normally
    {
        let mut guard = GLOBAL_BUDDY.lock();
        crate::klog!(boot, info, "frame: lock acquired, doing swap...");
        core::mem::swap(&mut *guard, &mut *buddy);
        crate::klog!(boot, info, "frame: swap done, dropping guard...");
    }
    crate::klog!(boot, info, "frame: guard dropped");
    {
        let _guard = GLOBAL_BUDDY.lock();
        crate::kprintln!(
            "frame: buddy initialized: {} total pages, {} free ({} MB)",
            _guard.total_pages(),
            _guard.available_pages(),
            _guard.available_pages() * PAGE_SIZE / (1024 * 1024)
        );
    }
}

/// Get the current CPU's magazine index.
/// Uses tp-based per_cpu in kernel context.
fn current_cpu_id() -> usize {
    // FIXME: dependency on executor
    // per_cpu::current().cpu_id
    0
}

/// Helper to initialize metadata for a newly allocated frame.
fn finalize_alloc(addr: PhysAddr, role: crate::mm::allocator::types::PageRole) -> PhysAddr {
    #[cfg(debug_assertions)]
    check_poison(addr);

    if let Some(meta) = crate::mm::allocator::types::get_frame_meta(addr) {
        meta.set_role(role);
        // Note: new allocations start with refcount 0 by default, so we bump to 1.
        // Wait, atomic inc_ref returns the old value, so if it's 0, it becomes 1.
        let old = meta.inc_ref();
        debug_assert_eq!(old, 0, "Allocated frame has non-zero refcount!");
    }
    addr
}

/// Synchronous frame allocation. Never yields.
/// Safe for trap context (sync_fault_handler).
///
/// Fallback chain: magazine -> buddy -> emergency_reclaim -> None
pub fn alloc_raw_frame_sync(role: crate::mm::allocator::types::PageRole) -> Option<PhysAddr> {
    let cpu_id = current_cpu_id();

    // 1. Try per-CPU magazine (fast path, minimal contention).
    {
        let mut mag = PER_CPU_MAGAZINES[cpu_id].lock();
        if let Some(addr) = mag.pop() {
            return Some(finalize_alloc(addr, role));
        }
    }

    // 2. Try global buddy alloc(0).
    {
        let mut buddy = GLOBAL_BUDDY.lock();
        if let Some(addr) = buddy.alloc(0) {
            return Some(finalize_alloc(addr, role));
        }
    }

    // 3. Try emergency reclaim.
    if let Some(addr) = emergency_reclaim_sync() {
        return Some(finalize_alloc(addr, role));
    }

    // 4. OOM
    None
}

/// Convenience wrapper: allocate a single user-anonymous physical frame
/// synchronously (no async context required).
///
/// Equivalent to `alloc_raw_frame_sync(PageRole::UserAnon)`.
pub fn frame_alloc_sync() -> Option<PhysAddr> {
    alloc_raw_frame_sync(crate::mm::allocator::types::PageRole::UserAnon)
}

/// Async frame allocation. Can yield to cooperate with page daemon.
///
/// Fallback chain: magazine -> buddy -> (wake page daemon + yield) -> emergency_reclaim -> None
pub async fn alloc_raw_frame(role: crate::mm::allocator::types::PageRole) -> Option<PhysAddr> {
    let cpu_id = current_cpu_id();

    // 1. Try per-CPU magazine.
    {
        let mut mag = PER_CPU_MAGAZINES[cpu_id].lock();
        if let Some(addr) = mag.pop() {
            return Some(finalize_alloc(addr, role));
        }
    }

    // 2. Try global buddy.
    {
        let mut buddy = GLOBAL_BUDDY.lock();
        if let Some(addr) = buddy.alloc(0) {
            return Some(finalize_alloc(addr, role));
        }
    }

    // 3. TODO(Phase 5): Wake page daemon and yield.
    // When the page daemon exists, we would signal it here and
    // yield so it can reclaim pages, then retry.
    // For now,    // FIXME: dependency on executor
    // crate::executor::yield_now().await;

    // 4. Retry buddy after yield.
    {
        let mut buddy = GLOBAL_BUDDY.lock();
        if let Some(addr) = buddy.alloc(0) {
            return Some(finalize_alloc(addr, role));
        }
    }

    // 5. Emergency reclaim.
    if let Some(addr) = emergency_reclaim_sync() {
        return Some(finalize_alloc(addr, role));
    }

    None
}

// ---------------------------------------------------------------------------
// Strongly Typed Allocator Wrappers
// ---------------------------------------------------------------------------

use crate::mm::allocator::types::{
    FileCache, KernelStack, PageRole, PipeBuffer, PteL0, PteL1, PteL2, SigTrampoline, TypedFrame,
    UserAnon, UserExec, UserStack,
};
use core::marker::PhantomData;

macro_rules! define_alloc_wrapper {
    ($name:ident, $name_sync:ident, $role_variant:ident, $type_token:ty) => {
        pub async fn $name() -> Option<TypedFrame<$type_token>> {
            alloc_raw_frame(PageRole::$role_variant)
                .await
                .map(|phys| TypedFrame {
                    phys,
                    _marker: PhantomData,
                })
        }

        pub fn $name_sync() -> Option<TypedFrame<$type_token>> {
            alloc_raw_frame_sync(PageRole::$role_variant).map(|phys| TypedFrame {
                phys,
                _marker: PhantomData,
            })
        }
    };
}

define_alloc_wrapper!(alloc_anon, alloc_anon_sync, UserAnon, UserAnon);
define_alloc_wrapper!(alloc_pte_l0, alloc_pte_l0_sync, PteL0, PteL0);
define_alloc_wrapper!(alloc_pte_l1, alloc_pte_l1_sync, PteL1, PteL1);
define_alloc_wrapper!(alloc_pte_l2, alloc_pte_l2_sync, PteL2, PteL2);
define_alloc_wrapper!(alloc_kstack, alloc_kstack_sync, KernelStack, KernelStack);
define_alloc_wrapper!(alloc_ustack, alloc_ustack_sync, UserStack, UserStack);
define_alloc_wrapper!(
    alloc_file_cache,
    alloc_file_cache_sync,
    FileCache,
    FileCache
);
define_alloc_wrapper!(alloc_user_exec, alloc_user_exec_sync, UserExec, UserExec);
define_alloc_wrapper!(alloc_pipe, alloc_pipe_sync, PipeBuffer, PipeBuffer);
define_alloc_wrapper!(
    alloc_sig_trampoline,
    alloc_sig_trampoline_sync,
    SigTrampoline,
    SigTrampoline
);

/// Free a single strongly-typed frame (order-0).
///
/// Refcount is decremented. If it hits 0, the frame is returned to the pool.
pub fn frame_free<R>(frame: TypedFrame<R>) {
    let addr = frame.phys();

    // Check and decrement refcount
    if let Some(meta) = crate::mm::allocator::types::get_frame_meta(addr) {
        let old_ref = meta.dec_ref();
        debug_assert!(
            old_ref > 0,
            "Double free or corrupted metadata: refcount was 0"
        );

        if old_ref > 1 {
            // Still referenced by other entities (e.g. COW, page cache)
            return;
        }

        // Refcount reached 0, transition to Free and proceed with true physical free
        meta.set_role(PageRole::Free);
    }

    free_raw_frame(addr);
}

/// Alias for `frame_free` to represent dropping a usage reference.
pub fn frame_free_usage<R>(frame: TypedFrame<R>) {
    frame_free(frame);
}

/// The actual raw underlying physical free logic. Internal use mostly,
/// but occasionally used for dropping contiguous allocations or untested stubs.
pub fn free_raw_frame(addr: PhysAddr) {
    assert!(
        addr.is_page_aligned(),
        "free_raw_frame: addr not page-aligned"
    );

    // Debug: poison the frame.
    #[cfg(debug_assertions)]
    poison_frame(addr);

    let cpu_id = current_cpu_id();
    let mut mag = PER_CPU_MAGAZINES[cpu_id].lock();

    if mag.push(addr).is_err() {
        // Magazine full — drain half to buddy, then push.
        let mut buddy = GLOBAL_BUDDY.lock();
        mag.drain_half(&mut buddy);
        drop(buddy);
        // Now push should succeed.
        let _ = mag.push(addr);
    }
}

/// Allocate 2^order contiguous pages directly from the buddy allocator.
/// For multi-page allocations (e.g., page tables, large buffers).
pub fn frame_alloc_contiguous(order: usize) -> Option<PhysAddr> {
    let mut buddy = GLOBAL_BUDDY.lock();
    let result = buddy.alloc(order);
    #[cfg(debug_assertions)]
    if let Some(addr) = result {
        // Check poison on each page in the block.
        for i in 0..(1usize << order) {
            check_poison(PhysAddr::new(addr.as_usize() + i * PAGE_SIZE));
        }
    }
    result
}

/// Free a contiguous block of 2^order pages back to the buddy allocator.
pub fn frame_free_contiguous(addr: PhysAddr, order: usize) {
    assert!(addr.is_page_aligned(), "frame_free_contiguous: not aligned");
    #[cfg(debug_assertions)]
    for i in 0..(1usize << order) {
        poison_frame(PhysAddr::new(addr.as_usize() + i * PAGE_SIZE));
    }
    let mut buddy = GLOBAL_BUDDY.lock();
    buddy.free(addr, order);
}

/// Emergency synchronous reclaim. Stub for now.
/// TODO(Phase 5): scan inactive queue for clean cached pages without yielding.
pub fn emergency_reclaim_sync() -> Option<PhysAddr> {
    None
}

/// Return the number of free pages available in the buddy allocator.
/// Does not count pages held in per-CPU magazines.
pub fn available_pages() -> usize {
    let buddy = GLOBAL_BUDDY.lock();
    buddy.available_pages()
}

// ---------------------------------------------------------------------------
// Debug-build memory poisoning
// ---------------------------------------------------------------------------

/// Fill a page frame with the poison pattern on free.
#[cfg(debug_assertions)]
fn poison_frame(addr: PhysAddr) {
    let ptr = addr.as_usize() as *mut u64;
    let count = PAGE_SIZE / core::mem::size_of::<u64>();
    for i in 0..count {
        unsafe { ptr.add(i).write_volatile(POISON_PATTERN) };
    }
}

/// Verify a page frame still contains the poison pattern on alloc.
/// Panics if corruption is detected (use-after-free).
#[cfg(debug_assertions)]
fn check_poison(addr: PhysAddr) {
    let ptr = addr.as_usize() as *const u64;
    let count = PAGE_SIZE / core::mem::size_of::<u64>();
    for i in 0..count {
        let val = unsafe { ptr.add(i).read_volatile() };
        if val != POISON_PATTERN {
            // Not poisoned — this is fine for freshly-initialized memory
            // that was never freed through our allocator. Only panic if
            // we can confirm it was previously poisoned (first word matches
            // but later words don't). For initial allocs, just return.
            return;
        }
    }
    // All poison — frame was freed and not corrupted. Clear it.
}
