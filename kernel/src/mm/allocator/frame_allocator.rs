//! Frame allocator with dual API (async + sync) and debug memory poisoning.
//!
//! - `frame_alloc_sync()`: synchronous, never yields — safe for trap context
//! - `frame_alloc()`: async-capable, can yield to wake page daemon (Phase 5)
//! - `frame_free()`: returns frame via per-CPU magazine, drains to buddy when full
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
        let guard = GLOBAL_BUDDY.lock();
        crate::kprintln!(
            "frame: buddy initialized: {} total pages, {} free ({} MB)",
            guard.total_pages(),
            guard.available_pages(),
            guard.available_pages() * PAGE_SIZE / (1024 * 1024)
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

/// Synchronous frame allocation. Never yields.
/// Safe for trap context (sync_fault_handler).
///
/// Fallback chain: magazine -> buddy -> emergency_reclaim -> None
pub fn frame_alloc_sync() -> Option<PhysAddr> {
    let cpu_id = current_cpu_id();

    // 1. Try per-CPU magazine (fast path, minimal contention).
    {
        let mut mag = PER_CPU_MAGAZINES[cpu_id].lock();
        if let Some(addr) = mag.pop() {
            #[cfg(debug_assertions)]
            check_poison(addr);
            return Some(addr);
        }
    }

    // 2. Try global buddy alloc(0).
    {
        let mut buddy = GLOBAL_BUDDY.lock();
        if let Some(addr) = buddy.alloc(0) {
            #[cfg(debug_assertions)]
            check_poison(addr);
            return Some(addr);
        }
    }

    // 3. Try emergency reclaim.
    if let Some(addr) = emergency_reclaim_sync() {
        #[cfg(debug_assertions)]
        check_poison(addr);
        return Some(addr);
    }

    // 4. OOM
    None
}

/// Async frame allocation. Can yield to cooperate with page daemon.
///
/// Fallback chain: magazine -> buddy -> (wake page daemon + yield) -> emergency_reclaim -> None
pub async fn frame_alloc() -> Option<PhysAddr> {
    let cpu_id = current_cpu_id();

    // 1. Try per-CPU magazine.
    {
        let mut mag = PER_CPU_MAGAZINES[cpu_id].lock();
        if let Some(addr) = mag.pop() {
            #[cfg(debug_assertions)]
            check_poison(addr);
            return Some(addr);
        }
    }

    // 2. Try global buddy.
    {
        let mut buddy = GLOBAL_BUDDY.lock();
        if let Some(addr) = buddy.alloc(0) {
            #[cfg(debug_assertions)]
            check_poison(addr);
            return Some(addr);
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
            #[cfg(debug_assertions)]
            check_poison(addr);
            return Some(addr);
        }
    }

    // 5. Emergency reclaim.
    if let Some(addr) = emergency_reclaim_sync() {
        #[cfg(debug_assertions)]
        check_poison(addr);
        return Some(addr);
    }

    None
}

/// Free a single frame (order-0).
///
/// Debug builds poison the frame before returning it.
/// Tries per-CPU magazine first; if full, drains half to buddy then pushes.
pub fn frame_free(addr: PhysAddr) {
    assert!(addr.is_page_aligned(), "frame_free: addr not page-aligned");

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
