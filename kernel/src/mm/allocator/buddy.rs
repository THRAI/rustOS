//! Buddy system frame allocator.
//!
//! Manages physical page frames with power-of-2 splitting and coalescing.
//! MAX_ORDER=11 supports up to 2^11 = 2048 contiguous pages (8MB blocks).

extern crate alloc;

use alloc::vec::Vec;
use hal_common::addr::{PhysAddr, PAGE_SIZE};

/// Maximum buddy order (2^11 = 2048 pages = 8MB).
pub const MAX_ORDER: usize = 11;

/// Buddy system allocator for physical page frames.
///
/// Each order `k` has a free list of blocks containing `2^k` contiguous pages.
/// Allocation splits larger blocks; freeing coalesces with the buddy if free.
pub struct BuddyAllocator {
    /// Free lists per order. `free_lists[k]` holds blocks of 2^k pages.
    free_lists: [Vec<PhysAddr>; MAX_ORDER + 1],
    /// Total pages managed (including allocated).
    total_pages: usize,
    /// Currently free pages.
    free_pages: usize,
}

impl BuddyAllocator {
    /// Create an empty buddy allocator.
    pub const fn new() -> Self {
        // const-init: we build the Vec array at runtime in init()
        Self {
            free_lists: [
                Vec::new(), Vec::new(), Vec::new(), Vec::new(),
                Vec::new(), Vec::new(), Vec::new(), Vec::new(),
                Vec::new(), Vec::new(), Vec::new(), Vec::new(),
            ],
            total_pages: 0,
            free_pages: 0,
        }
    }

    /// Initialize with a physical memory range [start, end).
    /// Both addresses must be page-aligned.
    pub fn init(&mut self, start: PhysAddr, end: PhysAddr) {
        assert!(start.is_page_aligned(), "buddy init: start not page-aligned");
        assert!(end.is_page_aligned(), "buddy init: end not page-aligned");
        assert!(start.as_usize() < end.as_usize(), "buddy init: empty range");

        let mut addr = start.as_usize();
        let end_addr = end.as_usize();
        let total = (end_addr - addr) / PAGE_SIZE;
        self.total_pages += total;

        // Greedily insert largest possible blocks.
        while addr < end_addr {
            let pages_left = (end_addr - addr) / PAGE_SIZE;
            // Find the largest order that:
            // 1. fits in remaining space
            // 2. is naturally aligned (addr is aligned to 2^order pages)
            let mut order = MAX_ORDER;
            while order > 0 {
                let block_pages = 1 << order;
                if block_pages <= pages_left
                    && (addr / PAGE_SIZE) % block_pages == 0
                {
                    break;
                }
                order -= 1;
            }
            self.free_lists[order].push(PhysAddr::new(addr));
            self.free_pages += 1 << order;
            addr += (1 << order) * PAGE_SIZE;
        }
    }

    /// Allocate a block of 2^order contiguous pages.
    /// Returns the base physical address of the block, or None if OOM.
    pub fn alloc(&mut self, order: usize) -> Option<PhysAddr> {
        assert!(order <= MAX_ORDER, "buddy alloc: order too large");

        // Find the smallest order with a free block >= requested.
        for current_order in order..=MAX_ORDER {
            if let Some(block) = self.free_lists[current_order].pop() {
                // Split down to the requested order.
                let mut split_order = current_order;
                let addr = block.as_usize();
                while split_order > order {
                    split_order -= 1;
                    // The upper half becomes a free buddy at split_order.
                    let buddy_addr = addr + ((1 << split_order) * PAGE_SIZE);
                    self.free_lists[split_order].push(PhysAddr::new(buddy_addr));
                }
                self.free_pages -= 1 << order;
                return Some(PhysAddr::new(addr));
            }
        }
        None
    }

    /// Free a block of 2^order contiguous pages starting at `addr`.
    /// Coalesces with the buddy if the buddy is also free.
    pub fn free(&mut self, addr: PhysAddr, order: usize) {
        assert!(order <= MAX_ORDER, "buddy free: order too large");
        assert!(addr.is_page_aligned(), "buddy free: addr not page-aligned");

        let mut current_addr = addr.as_usize();
        let mut current_order = order;

        // Try to coalesce with buddy at each level.
        while current_order < MAX_ORDER {
            let page_num = current_addr / PAGE_SIZE;
            let buddy_page = page_num ^ (1 << current_order);
            let buddy_addr = buddy_page * PAGE_SIZE;

            // Search for buddy in the free list at this order.
            let buddy_phys = PhysAddr::new(buddy_addr);
            if let Some(pos) = self.free_lists[current_order]
                .iter()
                .position(|&a| a == buddy_phys)
            {
                // Remove buddy from free list (swap_remove for O(1)).
                self.free_lists[current_order].swap_remove(pos);
                // Merge: the combined block starts at the lower address.
                current_addr = current_addr.min(buddy_addr);
                current_order += 1;
            } else {
                break;
            }
        }

        self.free_lists[current_order].push(PhysAddr::new(current_addr));
        self.free_pages += 1 << order;
    }

    /// Total free pages across all orders.
    pub fn available_pages(&self) -> usize {
        self.free_pages
    }

    /// Total pages managed by this allocator.
    pub fn total_pages(&self) -> usize {
        self.total_pages
    }
}
