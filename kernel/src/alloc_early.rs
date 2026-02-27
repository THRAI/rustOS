//! Kernel heap allocator backed by a static .bss buffer.
//!
//! Uses linked_list_allocator::Heap wrapped in our IrqSafeSpinLock.
//! Must be initialized (init_heap) before any allocation — call right after UART.

use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::Heap;
use hal_common::IrqSafeSpinLock;

const HEAP_SIZE: usize = 16 * 1024 * 1024; // 16MB
static mut HEAP_SPACE: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

static HEAP: IrqSafeSpinLock<Heap> = IrqSafeSpinLock::new(Heap::empty());

struct KernelAllocator;

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HEAP.lock()
            .allocate_first_fit(layout)
            .ok()
            .map_or(core::ptr::null_mut(), |nn| nn.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HEAP.lock()
            .deallocate(core::ptr::NonNull::new_unchecked(ptr), layout);
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

/// Initialize the kernel heap from a static buffer.
/// Must be called before anything that allocates (buddy init, per-CPU, etc.).
pub fn init_heap() {
    unsafe {
        HEAP.lock().init(HEAP_SPACE.as_mut_ptr(), HEAP_SIZE);
    }
    crate::klog!(boot, info, "heap initialized {} KB (static)", HEAP_SIZE / 1024);
}
