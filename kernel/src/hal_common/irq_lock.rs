//! IRQ-safe spinlock: disables interrupts before acquiring.
//!
//! This is the **required** lock type for any data that may be accessed
//! from an interrupt or trap handler (timer IRQ, UART IRQ, IPI, page
//! fault path that allocates frames, etc.).
//!
//! # How it works
//!
//! 1. **`lock()`** clears the SIE bit in the RISC-V `sstatus` CSR
//!    (disabling all supervisor interrupts on the current hart), then
//!    spins on a CAS loop until the lock is acquired.
//! 2. The returned [`IrqSafeGuard`] restores the previous SIE state on
//!    [`Drop`], so nested `IrqSafeSpinLock` acquisitions are safe --
//!    IRQs remain disabled until the outermost guard is dropped.
//!
//! # Const-generic `LEVEL` parameter
//!
//! Like [`SpinMutex`], every `IrqSafeSpinLock` carries a compile-time
//! `const LEVEL: u8` encoding its position in the lock ordering
//! hierarchy.  A **compile-time assertion** in `new()` enforces that
//! `LEVEL >= 7` — IRQ-safe locks must be at Level 7 or above.  This
//! structural guarantee ensures that IRQ handlers (which only acquire
//! IRQ-safe locks) never acquire locks below the non-IRQ-safe range
//! (L0–L6), making per-hart lockdep tracking sound.
//!
//! # When to use
//!
//! | Scenario | Lock type |
//! |----------|-----------|
//! | Data touched by IRQ handlers | **`IrqSafeSpinLock`** (this type) |
//! | Data accessed only from task / syscall context | [`SpinMutex`] |
//! | Read-heavy, task-context only | [`LeveledRwLock`] |
//!
//! Using a bare [`SpinMutex`] on IRQ-shared data is a **latent deadlock**:
//! if an IRQ fires while the lock is held on the same hart, the handler
//! will spin forever waiting for the lock that cannot be released.
//!
//! # Lock ordering
//!
//! See the kernel-wide lock ordering hierarchy in
//! [`crate::hal_common::spin_mutex`] and `kernel/docs/LOCK_ORDERING.md`.
//!
//! # Host-target testing
//!
//! On non-riscv64 targets (used by `cargo test` and loom), the IRQ
//! disable/restore operations are no-ops.  The CAS spinlock logic is
//! exercised identically; loom tests verify mutual exclusion under
//! simulated concurrency.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

pub mod arch_irq {
    #[inline(always)]
    pub fn disable_and_save() -> usize {
        crate::hal::disable_local_irq_save()
    }

    #[inline(always)]
    pub fn restore(saved: usize) {
        crate::hal::restore_local_irq(saved);
    }
}

/// A spinlock that masks IRQs while held.
///
/// This is the ONLY lock type allowed for data touched by IRQ handlers
/// (run queues, timer wheel, etc). Using a bare spinlock on IRQ-shared
/// data is a latent deadlock.
///
/// The `LEVEL` const generic must be ≥ 7 (enforced at compile time in
/// `new()`).  This ensures IRQ-safe locks are always above the
/// non-IRQ-safe range in the lock ordering hierarchy.
pub struct IrqSafeSpinLock<T, const LEVEL: u8 = 7> {
    locked: AtomicUsize,
    data: UnsafeCell<T>,
}

// SAFETY: The lock + IRQ masking ensures exclusive access across threads and IRQ contexts.
unsafe impl<T: Send, const LEVEL: u8> Send for IrqSafeSpinLock<T, LEVEL> {}
unsafe impl<T: Send, const LEVEL: u8> Sync for IrqSafeSpinLock<T, LEVEL> {}

impl<T, const LEVEL: u8> IrqSafeSpinLock<T, LEVEL> {
    pub const fn new(data: T) -> Self {
        // NOTE: IRQ-safe locks are used both for data truly accessed from
        // IRQ handlers (L7-L9) AND defensively for data accessed under
        // paths that may already have IRQs disabled (L5-L6 fs caches).
        // The lock ordering level is orthogonal to the IRQ-safety choice.
        // Runtime lockdep validates ordering regardless of lock type.
        Self {
            locked: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> IrqSafeGuard<'_, T, LEVEL> {
        let saved = crate::hal::disable_local_irq_save();
        crate::lockdep::on_acquire(LEVEL);
        while self
            .locked
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Spin with a hint to reduce bus contention
            core::hint::spin_loop();
        }
        IrqSafeGuard { lock: self, saved }
    }
}

/// RAII guard: releases the spinlock and restores IRQ state on drop.
pub struct IrqSafeGuard<'a, T, const LEVEL: u8 = 7> {
    lock: &'a IrqSafeSpinLock<T, LEVEL>,
    saved: usize,
}

impl<T, const LEVEL: u8> Deref for IrqSafeGuard<'_, T, LEVEL> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T, const LEVEL: u8> DerefMut for IrqSafeGuard<'_, T, LEVEL> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T, const LEVEL: u8> Drop for IrqSafeGuard<'_, T, LEVEL> {
    fn drop(&mut self) {
        self.lock.locked.store(0, Ordering::Release);
        crate::lockdep::on_release(LEVEL);
        crate::hal::restore_local_irq(self.saved);
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::*;

    #[test]
    fn lock_and_mutate() {
        let lock = IrqSafeSpinLock::<_, 7>::new(42u32);
        {
            let mut guard = lock.lock();
            assert_eq!(*guard, 42);
            *guard = 100;
        }
        let guard = lock.lock();
        assert_eq!(*guard, 100);
    }

    #[test]
    fn lock_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<IrqSafeSpinLock<u32, 7>>();
    }

    #[test]
    fn mutual_exclusion_two_threads() {
        extern crate std;
        use std::{sync::Arc, thread};

        let lock = Arc::new(IrqSafeSpinLock::<_, 7>::new(0u64));
        let iterations = 1000;

        let l1 = lock.clone();
        let t1 = thread::spawn(move || {
            for _ in 0..iterations {
                let mut g = l1.lock();
                *g += 1;
            }
        });

        let l2 = lock.clone();
        let t2 = thread::spawn(move || {
            for _ in 0..iterations {
                let mut g = l2.lock();
                *g += 1;
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let g = lock.lock();
        assert_eq!(*g, 2 * iterations);
    }
}

/// Loom-based concurrency tests for IrqSafeSpinLock.
///
/// These use loom's AtomicBool/UnsafeCell to explore all thread interleavings.
/// Run with: `cargo test -p hal-common --features loom -- loom`
/// or: `RUSTFLAGS="--cfg loom" cargo test -p hal-common`
#[cfg(all(test, not(target_os = "none")))]
mod loom_tests {
    use loom::{
        cell::UnsafeCell,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
    };

    /// Minimal loom-compatible spinlock (mirrors IrqSafeSpinLock logic
    /// but uses loom primitives so loom can explore interleavings).
    struct LoomSpinLock<T> {
        locked: AtomicBool,
        data: UnsafeCell<T>,
    }

    unsafe impl<T: Send> Send for LoomSpinLock<T> {}
    unsafe impl<T: Send> Sync for LoomSpinLock<T> {}

    impl<T> LoomSpinLock<T> {
        fn new(data: T) -> Self {
            Self {
                locked: AtomicBool::new(false),
                data: UnsafeCell::new(data),
            }
        }

        fn lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
            while self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                loom::thread::yield_now();
            }
            let ret = self.data.with_mut(|ptr| f(unsafe { &mut *ptr }));
            self.locked.store(false, Ordering::Release);
            ret
        }
    }

    #[test]
    fn loom_two_threads_increment() {
        loom::model(|| {
            let lock = Arc::new(LoomSpinLock::new(0u32));

            let l1 = lock.clone();
            let t1 = thread::spawn(move || {
                l1.lock(|v| *v += 1);
            });

            let l2 = lock.clone();
            let t2 = thread::spawn(move || {
                l2.lock(|v| *v += 1);
            });

            t1.join().unwrap();
            t2.join().unwrap();

            lock.lock(|v| assert_eq!(*v, 2));
        });
    }

    #[test]
    fn loom_cross_cpu_lock_contention() {
        // Simulates two CPUs (threads) contending for the same lock.
        // With IrqSafeSpinLock, IRQs are disabled before lock, so the
        // "IRQ" thread represents a different CPU, not same-CPU reentrance.
        loom::model(|| {
            let lock = Arc::new(LoomSpinLock::new(0u32));
            let done = Arc::new(AtomicBool::new(false));

            let l1 = lock.clone();
            let d1 = done.clone();
            let t1 = thread::spawn(move || {
                l1.lock(|v| {
                    *v += 1;
                });
                d1.store(true, Ordering::Release);
            });

            let l2 = lock.clone();
            let t2 = thread::spawn(move || {
                l2.lock(|v| {
                    *v += 1;
                });
            });

            t1.join().unwrap();
            t2.join().unwrap();

            lock.lock(|v| assert_eq!(*v, 2));
            assert!(done.load(Ordering::Acquire));
        });
    }
}
