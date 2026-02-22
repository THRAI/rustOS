//! Per-CPU magazine cache for single-frame (order-0) fast path.
//!
//! Each CPU has its own Magazine, eliminating global lock contention
//! on the hot path. When the magazine is empty, it refills from the
//! buddy allocator in batch. When full, it drains half back to buddy.

use hal_common::addr::PhysAddr;
use super::buddy::BuddyAllocator;

/// Number of frames a magazine can hold.
const MAGAZINE_CAPACITY: usize = 32;

/// Fixed-size stack of physical frame addresses.
/// Per-CPU, so no locking needed for pop/push.
pub struct Magazine {
    frames: [Option<PhysAddr>; MAGAZINE_CAPACITY],
    /// Index of the next free slot (also == count of frames held).
    count: usize,
}

impl Magazine {
    /// Create an empty magazine.
    pub const fn new() -> Self {
        Self {
            frames: [None; MAGAZINE_CAPACITY],
            count: 0,
        }
    }

    /// Take a frame from the magazine. O(1), no lock.
    pub fn pop(&mut self) -> Option<PhysAddr> {
        if self.count == 0 {
            return None;
        }
        self.count -= 1;
        self.frames[self.count].take()
    }

    /// Return a frame to the magazine. Err if full.
    pub fn push(&mut self, addr: PhysAddr) -> Result<(), PhysAddr> {
        if self.count >= MAGAZINE_CAPACITY {
            return Err(addr);
        }
        self.frames[self.count] = Some(addr);
        self.count += 1;
        Ok(())
    }

    /// Batch-refill from the buddy allocator.
    /// Tries to allocate `count` order-0 frames.
    pub fn refill(&mut self, buddy: &mut BuddyAllocator, count: usize) {
        let to_fill = count.min(MAGAZINE_CAPACITY - self.count);
        for _ in 0..to_fill {
            if let Some(addr) = buddy.alloc(0) {
                self.frames[self.count] = Some(addr);
                self.count += 1;
            } else {
                break;
            }
        }
    }

    /// Drain all frames back to the buddy allocator.
    pub fn drain(&mut self, buddy: &mut BuddyAllocator) {
        while self.count > 0 {
            self.count -= 1;
            if let Some(addr) = self.frames[self.count].take() {
                buddy.free(addr, 0);
            }
        }
    }

    /// Drain half the frames back to buddy (used when magazine is full).
    pub fn drain_half(&mut self, buddy: &mut BuddyAllocator) {
        let to_drain = self.count / 2;
        for _ in 0..to_drain {
            self.count -= 1;
            if let Some(addr) = self.frames[self.count].take() {
                buddy.free(addr, 0);
            }
        }
    }

    /// Number of frames currently held.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the magazine is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether the magazine is full.
    pub fn is_full(&self) -> bool {
        self.count >= MAGAZINE_CAPACITY
    }
}
