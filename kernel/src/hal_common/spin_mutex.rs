//! RISC-V safe SpinMutex using AtomicUsize (not AtomicBool).
//!
//! # WARNING: NOT safe for IRQ-context data
//!
//! `SpinMutex` does **not** disable interrupts. If the protected data is
//! accessed from an interrupt or trap handler (timer IRQ, page fault, IPI,
//! UART IRQ), use [`IrqSafeSpinLock`] instead. Acquiring a `SpinMutex`
//! from task context while an IRQ handler on the same hart tries to acquire
//! the same lock **will deadlock** — IRQs are not masked.
//!
//! Safe for: data accessed only from syscall / task context (e.g. fd_table,
//! cwd, signal actions, mount table, symlink table).
//!
//! # Kernel lock ordering hierarchy
//!
//! Acquire outer-first; never invert. Two locks at the same level must
//! not be held simultaneously unless noted.
//!
//! ```text
//! Level 0 (outermost / coarsest — IRQ-safe):
//!   GLOBAL_BUDDY, PER_CPU_MAGAZINES          (frame_allocator.rs)
//!
//! Level 1:
//!   task.vm_map (SpinMutex<VmMap>)            (proc/task.rs)
//!   task.pmap   (Arc<SpinMutex<Pmap>>)        (mm/vm/map/mod.rs)
//!
//! Level 2:
//!   VmObject    (spin::RwLock<VmObject>)      (mm/vm/vm_object.rs)
//!
//! Level 3:
//!   task.trap_frame, task.fd_table,           (proc/task.rs)
//!   task.cwd, task.signals.actions
//!
//! Level 4:
//!   VNODE_OBJECTS  then  VNODE_LRU            (fs/vnode.rs — always this order)
//!   DENTRY_CACHE                             (fs/dentry.rs)
//!
//! Level 5:
//!   REQUEST_QUEUE, DELEGATE_WAKER,           (fs/delegate.rs)
//!   OneShotInner::{waker, value}
//!
//! Level 6 (innermost / finest):
//!   CONSOLE_INPUT                            (console.rs — IrqSafeSpinLock)
//!   PRINT_LOCK                               (console.rs — AtomicBool, avoids circular dep)
//!   FUTEX_TABLE                              (ipc/futex.rs)
//! ```
//!
//! **IRQ-context rule:** Any lock that MAY be acquired from an IRQ / trap
//! handler MUST use `IrqSafeSpinLock`. `SpinMutex` and `spin::RwLock`
//! are ONLY safe for data accessed exclusively from task / syscall context.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

/// A simple CAS spinlock **without** IRQ masking.
///
/// See the module-level documentation for safety constraints and the
/// kernel-wide lock ordering hierarchy.
pub struct SpinMutex<T> {
    locked: AtomicUsize,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for SpinMutex<T> {}
unsafe impl<T: Send> Sync for SpinMutex<T> {}

impl<T> SpinMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinMutexGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinMutexGuard { lock: self }
    }
}

pub struct SpinMutexGuard<'a, T> {
    lock: &'a SpinMutex<T>,
}

impl<T> Deref for SpinMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(0, Ordering::Release);
    }
}
