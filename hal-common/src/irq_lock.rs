//! IrqSafeSpinLock: a spinlock that disables IRQs before acquiring.
//!
//! On rv64, disable_and_save/restore use real CSR ops.
//! On host (for testing), they are no-ops.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Arch-specific IRQ control.
/// On host targets (x86_64, aarch64), these are no-ops for testability.
#[cfg(not(target_arch = "riscv64"))]
mod arch_irq {
    pub fn disable_and_save() -> usize {
        0
    }
    pub fn restore(_saved: usize) {}
}

#[cfg(target_arch = "riscv64")]
mod arch_irq {
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
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: The lock + IRQ masking ensures exclusive access across threads and IRQ contexts.
unsafe impl<T: Send> Send for IrqSafeSpinLock<T> {}
unsafe impl<T: Send> Sync for IrqSafeSpinLock<T> {}

impl<T> IrqSafeSpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> IrqSafeGuard<'_, T> {
        let saved = arch_irq::disable_and_save();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Spin with a hint to reduce bus contention
            core::hint::spin_loop();
        }
        IrqSafeGuard {
            lock: self,
            saved,
        }
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
        self.lock.locked.store(false, Ordering::Release);
        arch_irq::restore(self.saved);
    }
}

#[cfg(test)]
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
}
