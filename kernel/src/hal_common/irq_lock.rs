//! IrqSafeSpinLock: a spinlock that disables IRQs before acquiring.
//!
//! On rv64, disable_and_save/restore use real CSR ops.
//! On host (for testing), they are no-ops.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Arch-specific IRQ control.
/// On host targets (x86_64, aarch64), these are no-ops for testability.
#[cfg(not(target_arch = "riscv64"))]
pub mod arch_irq {
    pub fn disable_and_save() -> usize {
        0
    }
    pub fn restore(_saved: usize) {}
}

#[cfg(target_arch = "riscv64")]
pub mod arch_irq {
    /// Read sstatus, clear SIE bit, return old sstatus value.
    pub fn disable_and_save() -> usize {
        let saved: usize;
        unsafe {
            core::arch::asm!(
                "csrrci {}, sstatus, 0x2",
                out(reg) saved,
            );
        }
        saved
    }

    /// Restore SIE bit from saved sstatus value.
    pub fn restore(saved: usize) {
        // Only restore the SIE bit (bit 1)
        if saved & 0x2 != 0 {
            unsafe {
                core::arch::asm!("csrsi sstatus, 0x2");
            }
        }
    }
}

/// A spinlock that masks IRQs while held.
///
/// This is the ONLY lock type allowed for data touched by IRQ handlers
/// (run queues, timer wheel, etc). Using a bare spinlock on IRQ-shared
/// data is a latent deadlock.
pub struct IrqSafeSpinLock<T> {
    locked: AtomicUsize,
    data: UnsafeCell<T>,
}

// SAFETY: The lock + IRQ masking ensures exclusive access across threads and IRQ contexts.
unsafe impl<T: Send> Send for IrqSafeSpinLock<T> {}
unsafe impl<T: Send> Sync for IrqSafeSpinLock<T> {}

impl<T> IrqSafeSpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> IrqSafeGuard<'_, T> {
        let saved = arch_irq::disable_and_save();
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
pub struct IrqSafeGuard<'a, T> {
    lock: &'a IrqSafeSpinLock<T>,
    saved: usize,
}

impl<T> Deref for IrqSafeGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for IrqSafeGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for IrqSafeGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(0, Ordering::Release);
        arch_irq::restore(self.saved);
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::*;

    #[test]
    fn lock_and_mutate() {
        let lock = IrqSafeSpinLock::new(42u32);
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
        assert_send_sync::<IrqSafeSpinLock<u32>>();
    }

    #[test]
    fn mutual_exclusion_two_threads() {
        extern crate std;
        use std::sync::Arc;
        use std::thread;

        let lock = Arc::new(IrqSafeSpinLock::new(0u64));
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
    use loom::cell::UnsafeCell;
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::sync::Arc;
    use loom::thread;

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
