//! Minimal bump allocator for early boot.
//!
//! Provides a #[global_allocator] so hal-common's alloc-using modules link.
//! Will be replaced by a proper allocator when VM is available (Phase 2).

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::cell::UnsafeCell;

/// 64KB static heap for early boot allocations.
const HEAP_SIZE: usize = 64 * 1024;

#[repr(align(16))]
struct AlignedHeap(UnsafeCell<[u8; HEAP_SIZE]>);

// SAFETY: Access is serialized via atomic HEAP_POS compare-exchange.
unsafe impl Sync for AlignedHeap {}

static HEAP: AlignedHeap = AlignedHeap(UnsafeCell::new([0; HEAP_SIZE]));
static HEAP_POS: AtomicUsize = AtomicUsize::new(0);

struct BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        loop {
            let pos = HEAP_POS.load(Ordering::Relaxed);
            let aligned = (pos + layout.align() - 1) & !(layout.align() - 1);
            let new_pos = aligned + layout.size();
            if new_pos > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if HEAP_POS
                .compare_exchange_weak(pos, new_pos, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return unsafe { (*HEAP.0.get()).as_mut_ptr().add(aligned) };
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator never frees -- acceptable for early boot
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator;
