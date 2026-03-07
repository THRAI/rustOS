//! Single-layer timer wheel with O(1) insert/expire.
//!
//! 256 slots, 10ms tick granularity. Cancellation by marking.
//! The timer wheel itself is NOT wrapped in IrqSafeSpinLock --
//! the PerCpu struct does that, keeping this module pure and testable.

extern crate alloc;
use alloc::vec::Vec;
use core::task::Waker;

/// Number of slots in the timer wheel.
pub const SLOTS: usize = 256;

/// Tick granularity in milliseconds.
pub const TICK_MS: u64 = 10;

/// A single timer entry in the wheel.
struct TimerEntry {
    id: u64,
    deadline_tick: u64,
    waker: Option<Waker>,
    cancelled: bool,
}

/// Single-layer timer wheel.
///
/// Insert and expire are O(1) (amortized over expired entries).
/// Cancel marks an entry; it is skipped on expiry.
pub struct TimerWheel {
    slots: Vec<Vec<TimerEntry>>,
    current_tick: u64,
    next_id: u64,
}

impl TimerWheel {
    /// Create a new timer wheel with all slots empty.
    //TODO: It's used only once, how can we mute the default-needed warning? lazy_static? is it thread safe?
    pub fn new() -> Self {
        let mut slots = Vec::with_capacity(SLOTS);
        for _ in 0..SLOTS {
            slots.push(Vec::new());
        }
        Self {
            slots,
            current_tick: 0,
            next_id: 1,
        }
    }

    /// Insert a timer that expires after `delay_ms` milliseconds.
    /// Returns a timer ID for cancellation.
    /// A delay of 0 expires on the next tick (minimum 1 tick).
    pub fn insert(&mut self, delay_ms: u64, waker: Waker) -> u64 {
        let delay_ticks = core::cmp::max(1, delay_ms.div_ceil(TICK_MS));
        let deadline_tick = self.current_tick + delay_ticks;
        let slot = (deadline_tick as usize) % SLOTS;
        let id = self.next_id;
        self.next_id += 1;
        self.slots[slot].push(TimerEntry {
            id,
            deadline_tick,
            waker: Some(waker),
            cancelled: false,
        });
        id
    }

    /// Cancel a timer by ID. Sets the cancelled flag so it is skipped on expiry.
    pub fn cancel(&mut self, id: u64) {
        for slot in self.slots.iter_mut() {
            for entry in slot.iter_mut() {
                if entry.id == id {
                    entry.cancelled = true;
                    entry.waker = None;
                    return;
                }
            }
        }
    }

    /// Advance the internal tick counter by 1 and process the current slot.
    /// Wakes all non-cancelled entries whose deadline has been reached.
    pub fn advance(&mut self) {
        self.current_tick += 1;
        self.tick(self.current_tick);
    }

    /// Process all entries in the slot for `current_tick`.
    /// Wakes non-cancelled entries at or before the given tick, removes all processed.
    pub fn tick(&mut self, current_tick: u64) {
        let slot_idx = (current_tick as usize) % SLOTS;
        let slot = core::mem::take(&mut self.slots[slot_idx]);
        let mut remaining = Vec::new();
        for mut entry in slot {
            if entry.deadline_tick <= current_tick {
                if !entry.cancelled {
                    if let Some(waker) = entry.waker.take() {
                        waker.wake();
                    }
                }
                // Drop expired/cancelled entries
            } else {
                // Not yet expired -- put back
                remaining.push(entry);
            }
        }
        self.slots[slot_idx] = remaining;
    }

    /// Get the current tick value.
    pub fn current_tick(&self) -> u64 {
        self.current_tick
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    extern crate std;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Wake, Waker};

    /// A simple waker that increments a counter when woken.
    struct CountWaker(AtomicUsize);

    impl Wake for CountWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn make_waker() -> (Waker, Arc<CountWaker>) {
        let cw = Arc::new(CountWaker(AtomicUsize::new(0)));
        let waker = Waker::from(cw.clone());
        (waker, cw)
    }

    #[test]
    fn timer_expires_at_correct_tick() {
        let mut tw = TimerWheel::new();
        let (waker, counter) = make_waker();
        // 50ms = 5 ticks at 10ms granularity
        tw.insert(50, waker);

        // Advance 4 ticks (40ms) -- not expired yet
        for _ in 0..4 {
            tw.advance();
        }
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);

        // Advance 1 more tick (50ms) -- should expire
        tw.advance();
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancel_prevents_wake() {
        let mut tw = TimerWheel::new();
        let (waker, counter) = make_waker();
        let id = tw.insert(30, waker);
        tw.cancel(id);

        for _ in 0..5 {
            tw.advance();
        }
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn multiple_timers_same_slot() {
        let mut tw = TimerWheel::new();
        let (w1, c1) = make_waker();
        let (w2, c2) = make_waker();
        tw.insert(20, w1);
        tw.insert(20, w2);

        for _ in 0..2 {
            tw.advance();
        }
        assert_eq!(c1.0.load(Ordering::SeqCst), 1);
        assert_eq!(c2.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn zero_delay_expires_on_next_tick() {
        let mut tw = TimerWheel::new();
        let (waker, counter) = make_waker();
        // 0ms delay rounds up to 0 ticks, but insert adds at least
        // ceil(0/10) = 0 ticks from current. deadline = current_tick + 0 = 0.
        // But current_tick is 0, so slot 0. advance() moves to tick 1.
        // Actually: (0 + 10 - 1) / 10 = 0 ticks. deadline = 0.
        // tick(1) checks deadline <= 1, so it fires.
        tw.insert(0, waker);
        tw.advance();
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn timer_wraps_around_slots() {
        let mut tw = TimerWheel::new();
        let (waker, counter) = make_waker();
        // 256 * 10ms = 2560ms -- wraps around to slot 0
        tw.insert(2560, waker);

        for _ in 0..255 {
            tw.advance();
        }
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);

        tw.advance(); // tick 256
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }
}
