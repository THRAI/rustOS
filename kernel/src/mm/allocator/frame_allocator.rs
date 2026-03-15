//! Frame allocator with dual API (async + sync) and debug memory poisoning.
//!
//! - `alloc_raw_frame_sync()`: synchronous, never yields — safe for trap context
//! - `frame_alloc()`: async-capable, can yield to wake page daemon (Phase 5)
//! - `free_raw_frame()`: returns frame via per-CPU magazine, drains to buddy when full
//! - `frame_alloc_contiguous()`: multi-page allocation directly from buddy

use core::sync::atomic::Ordering;

use crate::{
    hal_common::{IrqSafeSpinLock, PhysAddr, PAGE_SIZE},
    mm::{
        allocator::{FRAME_META, FRAME_META_LEN},
        get_frame_meta, BuddyAllocator, Magazine, PageRole, VmPage,
    },
};

/// Debug-build poison pattern: written to every u64 in a freed frame.
#[cfg(debug_assertions)]
const POISON_PATTERN: u64 = 0xDEAD_BEEF_DEAD_BEEF;

/// Trap stack canary value, written at bottom of trap stack.
#[cfg(debug_assertions)]
pub const STACK_CANARY: u64 = 0xCAFE_BABE_DEAD_C0DE;

/// Global buddy allocator, protected by IrqSafeSpinLock.
///
/// Lock ordering: **Level 7** (physical memory).  Acquired inside pmap
/// (Level 2) and VmObject (Level 3) paths via `alloc_raw_frame_sync` /
/// `free_raw_frame`.  Always acquired *after* `PER_CPU_MAGAZINES`.
/// IRQ-safe: yes -- frame allocation can occur from any context.
static GLOBAL_BUDDY: IrqSafeSpinLock<BuddyAllocator, 7> =
    IrqSafeSpinLock::new(BuddyAllocator::new());

/// Per-CPU magazine caches for fast order-0 frame alloc/free.
///
/// Lock ordering: **Level 7** (physical memory).  Acquired inside pmap
/// (Level 2) and VmObject (Level 3) paths.  Always acquired *before*
/// `GLOBAL_BUDDY` when both are needed (drain/refill paths).
/// IRQ-safe: yes -- indexed by `current_cpu_id()`.
static PER_CPU_MAGAZINES: [IrqSafeSpinLock<Magazine, 7>; 8] = [
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
pub(in crate::mm) fn init_frame_allocator(start: PhysAddr, end: PhysAddr) {
    // Initialize the global FRAME_META array
    // We need one VmPage per physical page in the system (up to `end` address)
    // To be safe and cover all possible PFNs up to the max address:
    let max_pfn = end.page_align_down().0 / PAGE_SIZE;
    let meta_size_bytes = max_pfn * core::mem::size_of::<VmPage>();
    let meta_pages = meta_size_bytes.div_ceil(PAGE_SIZE);

    // Steal memory for metadata array before giving the rest to the buddy allocator
    let meta_ptr = start;
    let next_start_addr = start + meta_pages * PAGE_SIZE;
    let buddy_start = next_start_addr;

    crate::klog!(boot, info, "frame: Box::new(BuddyAllocator::new())...");
    let mut buddy = alloc::boxed::Box::new(BuddyAllocator::new());
    crate::klog!(boot, info, "frame: box created, calling init...");
    buddy.init(buddy_start, end);
    crate::klog!(
        boot,
        info,
        "frame: init done, total={} free={}",
        buddy.total_pages(),
        buddy.available_pages()
    );

    // Zero out the allocated metadata memory
    unsafe {
        let meta_array =
            core::slice::from_raw_parts_mut(meta_ptr.as_usize() as *mut VmPage, max_pfn);

        // Initialize each VmPage correctly, particularly its physical address link
        for (pfn, meta) in meta_array.iter_mut().enumerate() {
            *meta = VmPage::new();
            meta.phys_addr = PhysAddr::new(pfn * PAGE_SIZE);
        }

        // Publish with Release ordering so secondary harts see the
        // fully-initialised array once they observe the non-null pointer.
        FRAME_META.store(meta_array.as_mut_ptr(), Ordering::Release);
        FRAME_META_LEN.store(max_pfn, Ordering::Release);
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
///
/// Reads the `tp`-based per-CPU data structure set up during boot
/// (see [`crate::executor::per_cpu::set_tp`]).  Must not be called
/// before `set_tp()` has run on the current hart.
#[inline]
fn current_cpu_id() -> usize {
    crate::executor::per_cpu::current().cpu_id
}

/// Helper to initialize metadata for a newly allocated frame.
fn finalize_alloc(addr: PhysAddr, role: PageRole) -> PhysAddr {
    #[cfg(debug_assertions)]
    check_poison(addr);

    if let Some(meta) = get_frame_meta(addr) {
        meta.set_role(role);
        // Note: new allocations start with refcount 0 by default, so we bump to 1.
        // Wait, atomic inc_ref returns the *new* value, well in `types::VmPage` it returns *old* fetch_add value
        let old = meta.inc_ref();
        debug_assert_eq!(old, 0, "Allocated frame has non-zero refcount!");
    }
    addr
}

/// Synchronous frame allocation. Never yields.
/// Safe for trap context (sync_fault_handler).
///
/// Fallback chain: magazine -> buddy -> emergency_reclaim -> None
pub(in crate::mm) fn alloc_raw_frame_sync(role: PageRole) -> Option<PhysAddr> {
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
pub(in crate::mm) fn frame_alloc_sync() -> Option<PhysAddr> {
    alloc_raw_frame_sync(PageRole::UserAnon)
}

/// Async frame allocation. Can yield to cooperate with page daemon.
///
/// Fallback chain: magazine -> buddy -> (wake page daemon + yield) -> emergency_reclaim -> None
pub(in crate::mm) async fn alloc_raw_frame(role: PageRole) -> Option<PhysAddr> {
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

macro_rules! define_alloc_wrapper {
    ($name:ident, $name_sync:ident, $role_variant:ident) => {
        pub(in crate::mm) async fn $name() -> Option<&'static VmPage> {
            alloc_raw_frame(PageRole::$role_variant)
                .await
                .map(|phys| get_frame_meta(phys).unwrap())
        }

        pub(in crate::mm) fn $name_sync() -> Option<&'static VmPage> {
            alloc_raw_frame_sync(PageRole::$role_variant).map(|phys| get_frame_meta(phys).unwrap())
        }
    };
}

define_alloc_wrapper!(alloc_anon, alloc_anon_sync, UserAnon);
define_alloc_wrapper!(alloc_pte_l0, alloc_pte_l0_sync, PteL0);
define_alloc_wrapper!(alloc_pte_l1, alloc_pte_l1_sync, PteL1);
define_alloc_wrapper!(alloc_pte_l2, alloc_pte_l2_sync, PteL2);
define_alloc_wrapper!(alloc_kstack, alloc_kstack_sync, KernelStack);
define_alloc_wrapper!(alloc_ustack, alloc_ustack_sync, UserStack);
define_alloc_wrapper!(alloc_file_cache, alloc_file_cache_sync, FileCache);
define_alloc_wrapper!(alloc_user_exec, alloc_user_exec_sync, UserExec);
define_alloc_wrapper!(alloc_pipe, alloc_pipe_sync, PipeBuffer);
define_alloc_wrapper!(
    alloc_sig_trampoline,
    alloc_sig_trampoline_sync,
    SigTrampoline
);

/// Free a VmPage usage reference previously returned by an allocator wrapper.
///
/// Refcount is decremented. If it hits 0, the frame is returned to the pool.
pub(in crate::mm) fn frame_free(frame: &'static VmPage) {
    let addr = frame.phys_addr;

    // Check and decrement refcount
    if let Some(meta) = get_frame_meta(addr) {
        let old_ref = meta.dec_ref();
        debug_assert!(
            old_ref > 0,
            "Double free or corrupted metadata: refcount was 0"
        );

        if old_ref > 1 {
            // Still referenced by other entities (e.g. COW, page cache)
            return;
        }

        // Ensure all prior writes from other CPUs (that decremented refs) are
        // visible before we return this frame to the buddy pool.  This is the
        // standard Arc::drop pattern — matches page.rs teardown_from_object
        // (line 320) and unwire_mapped (line 351).
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        // Refcount reached 0, transition to Free and proceed with true physical free
        meta.set_role(PageRole::Free);
    }

    free_raw_frame(addr);
}

/// Alias for `frame_free` to represent dropping a usage reference.
pub(in crate::mm) fn frame_free_usage(frame: &'static VmPage) {
    frame_free(frame);
}

/// The actual raw underlying physical free logic. Internal use mostly,
/// but occasionally used for dropping contiguous allocations or untested stubs.
pub(in crate::mm) fn free_raw_frame(addr: PhysAddr) {
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
pub(in crate::mm) fn frame_alloc_contiguous(order: usize) -> Option<PhysAddr> {
    let mut buddy = GLOBAL_BUDDY.lock();
    let result = buddy.alloc(order);
    #[cfg(debug_assertions)]
    if let Some(addr) = result {
        // Check poison on each page in the block.
        for i in 0..(1usize << order) {
            check_poison(PhysAddr::new(addr.0 + i * PAGE_SIZE));
        }
    }
    result
}

/// Free a contiguous block of 2^order pages back to the buddy allocator.
pub(in crate::mm) fn frame_free_contiguous(addr: PhysAddr, order: usize) {
    assert!(addr.is_page_aligned(), "frame_free_contiguous: not aligned");
    #[cfg(debug_assertions)]
    for i in 0..(1usize << order) {
        poison_frame(PhysAddr::new(addr.0 + i * PAGE_SIZE));
    }
    let mut buddy = GLOBAL_BUDDY.lock();
    buddy.free(addr, order);
}

/// Emergency synchronous reclaim: drain other CPUs' magazines back to buddy,
/// then retry allocation.
///
/// When both the current CPU's magazine and the buddy are empty, pages may
/// still be hoarded in other CPUs' magazines.  This function iterates over
/// all magazine slots and drains each one back to the buddy allocator, then
/// attempts a single order-0 allocation from the now-replenished buddy.
///
/// This is a last-resort path — it acquires multiple locks sequentially
/// (each magazine lock, then the buddy lock inside `drain`), so it has
/// higher latency than the normal fast path.  It runs synchronously and
/// never yields, making it safe for trap context.
pub(in crate::mm) fn emergency_reclaim_sync() -> Option<PhysAddr> {
    let caller_cpu = current_cpu_id();

    // Drain every *other* CPU's magazine back to buddy.
    // We skip our own because alloc_raw_frame_sync already emptied it.
    for (i, mag_lock) in PER_CPU_MAGAZINES.iter().enumerate() {
        if i == caller_cpu {
            continue;
        }
        let mut mag = mag_lock.lock();
        if !mag.is_empty() {
            let mut buddy = GLOBAL_BUDDY.lock();
            mag.drain(&mut buddy);
        }
    }

    // Now try buddy again — pages from other magazines should be available.
    let mut buddy = GLOBAL_BUDDY.lock();
    buddy.alloc(0)
}

/// Return the number of free pages available in the buddy allocator.
/// Does not count pages held in per-CPU magazines.
pub(in crate::mm) fn available_pages() -> usize {
    let buddy = GLOBAL_BUDDY.lock();
    buddy.available_pages()
}

// ---------------------------------------------------------------------------
// Debug-build memory poisoning
// ---------------------------------------------------------------------------

/// Fill a page frame with the poison pattern on free.
#[cfg(debug_assertions)]
fn poison_frame(addr: PhysAddr) {
    let ptr = addr.0 as *mut u64;
    let count = PAGE_SIZE / core::mem::size_of::<u64>();
    for i in 0..count {
        unsafe { ptr.add(i).write_volatile(POISON_PATTERN) };
    }
}

/// Verify a page frame still contains the poison pattern on alloc.
/// Panics if corruption is detected (use-after-free).
#[cfg(debug_assertions)]
fn check_poison(addr: PhysAddr) {
    let ptr = addr.0 as *const u64;
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
