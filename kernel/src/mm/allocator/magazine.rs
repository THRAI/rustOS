//! Per-CPU magazine cache for single-frame (order-0) fast path.
//!
//! Each CPU has its own Magazine, eliminating global lock contention
//! on the hot path. When the magazine is empty, it refills from the
//! buddy allocator in batch. When full, it drains half back to buddy.

use crate::{hal_common::PhysAddr, mm::BuddyAllocator};

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

#[cfg(all(test, feature = "qemu-test"))]
mod tests {
    use super::*;

    #[test]
    fn new_magazine_is_empty() {
        let m = Magazine::new();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert!(!m.is_full());
    }

    #[test]
    fn push_pop_single() {
        let mut m = Magazine::new();
        let addr = PhysAddr::new(0x1000);
        assert!(m.push(addr).is_ok());
        assert_eq!(m.len(), 1);
        let popped = m.pop().unwrap();
        assert_eq!(popped, addr);
        assert!(m.is_empty());
    }

    #[test]
    fn push_pop_lifo_order() {
        let mut m = Magazine::new();
        let a1 = PhysAddr::new(0x1000);
        let a2 = PhysAddr::new(0x2000);
        let a3 = PhysAddr::new(0x3000);
        m.push(a1).unwrap();
        m.push(a2).unwrap();
        m.push(a3).unwrap();
        assert_eq!(m.pop().unwrap(), a3);
        assert_eq!(m.pop().unwrap(), a2);
        assert_eq!(m.pop().unwrap(), a1);
    }

    #[test]
    fn push_full_returns_err() {
        let mut m = Magazine::new();
        for i in 0..MAGAZINE_CAPACITY {
            m.push(PhysAddr::new(i * 0x1000)).unwrap();
        }
        assert!(m.is_full());
        let overflow = PhysAddr::new(0xFFFF_0000);
        assert_eq!(m.push(overflow), Err(overflow));
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut m = Magazine::new();
        assert!(m.pop().is_none());
    }

    #[test]
    fn refill_from_buddy() {
        let mut buddy = BuddyAllocator::new();
        buddy.init(
            PhysAddr::new(0x8000_0000),
            PhysAddr::new(0x8000_0000 + 64 * 4096),
        );
        let mut m = Magazine::new();
        m.refill(&mut buddy, 8);
        assert_eq!(m.len(), 8);
    }

    #[test]
    fn drain_to_buddy() {
        let mut buddy = BuddyAllocator::new();
        buddy.init(
            PhysAddr::new(0x8000_0000),
            PhysAddr::new(0x8000_0000 + 64 * 4096),
        );
        let mut m = Magazine::new();
        m.refill(&mut buddy, 8);
        let before = buddy.available_pages();
        m.drain(&mut buddy);
        assert!(m.is_empty());
        assert_eq!(buddy.available_pages(), before + 8);
    }

    #[test]
    fn drain_half() {
        let mut buddy = BuddyAllocator::new();
        buddy.init(
            PhysAddr::new(0x8000_0000),
            PhysAddr::new(0x8000_0000 + 64 * 4096),
        );
        let mut m = Magazine::new();
        m.refill(&mut buddy, 10);
        m.drain_half(&mut buddy);
        assert_eq!(m.len(), 5);
    }
}
