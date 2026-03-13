//! Per-CPU run queue: FIFO queue protected by IrqSafeSpinLock.
//!
//! Generic over `R` so host tests can use simple types while the kernel
//! uses `async_task::Runnable`.

extern crate alloc;
use alloc::collections::VecDeque;

use super::IrqSafeSpinLock;

/// A FIFO run queue protected by IrqSafeSpinLock.
///
/// Every access disables IRQs first, preventing the IRQ-reentrance
/// deadlock that bare spinlocks leave latent.
pub struct RunQueue<R> {
    inner: IrqSafeSpinLock<VecDeque<R>, 8>,
}

impl<R> RunQueue<R> {
    /// Create an empty run queue.
    pub const fn new() -> Self {
        Self {
            inner: IrqSafeSpinLock::new(VecDeque::new()),
        }
    }

    /// Push an item to the back of the queue.
    pub fn push(&self, item: R) {
        self.inner.lock().push_back(item);
    }

    /// Pop an item from the front of the queue (FIFO).
    pub fn pop(&self) -> Option<R> {
        self.inner.lock().pop_front()
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Return the number of items in the queue.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }
}

// SAFETY: RunQueue is safe to share across threads because IrqSafeSpinLock
// provides mutual exclusion with IRQ masking.
unsafe impl<R: Send> Send for RunQueue<R> {}
unsafe impl<R: Send> Sync for RunQueue<R> {}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    extern crate std;
    use std::{sync::Arc, thread, vec::Vec};

    use super::*;

    #[test]
    fn push_pop_fifo_order() {
        let rq = RunQueue::new();
        for i in 0..10u32 {
            rq.push(i);
        }
        for i in 0..10u32 {
            assert_eq!(rq.pop(), Some(i));
        }
        assert_eq!(rq.pop(), None);
    }

    #[test]
    fn empty_queue_returns_none() {
        let rq: RunQueue<u32> = RunQueue::new();
        assert!(rq.is_empty());
        assert_eq!(rq.len(), 0);
        assert_eq!(rq.pop(), None);
    }

    #[test]
    fn len_tracks_items() {
        let rq = RunQueue::new();
        rq.push(1u32);
        rq.push(2);
        assert_eq!(rq.len(), 2);
        rq.pop();
        assert_eq!(rq.len(), 1);
    }

    #[test]
    fn push_from_multiple_threads() {
        let rq = Arc::new(RunQueue::new());
        let mut handles = Vec::new();
        let per_thread = 100;
        let num_threads = 4;

        for t in 0..num_threads {
            let rq = rq.clone();
            handles.push(thread::spawn(move || {
                for i in 0..per_thread {
                    rq.push(t * per_thread + i);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(rq.len(), num_threads * per_thread);

        let mut all = Vec::new();
        while let Some(v) = rq.pop() {
            all.push(v);
        }
        assert_eq!(all.len(), num_threads * per_thread);

        // Verify all values present (order may vary across threads)
        all.sort();
        let expected: Vec<usize> = (0..num_threads * per_thread).collect();
        assert_eq!(all, expected);
    }
}
