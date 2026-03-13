//! RISC-V safe SpinMutex using AtomicUsize (not AtomicBool).
//!
//! # WARNING: NOT safe for IRQ-context data
//!
//! `SpinMutex` does **not** disable interrupts. If the protected data is
//! accessed from an interrupt or trap handler (timer IRQ, page fault, IPI,
//! UART IRQ), use [`IrqSafeSpinLock`] instead. Acquiring a `SpinMutex`
//! from task context while an IRQ handler on the same hart tries to acquire
//! the same lock **will deadlock** -- IRQs are not masked.
//!
//! Safe for: data accessed only from syscall / task context (e.g. fd_table,
//! cwd, signal actions, mount table, symlink table, VirtIO driver).
//!
//! # Const-generic `LEVEL` parameter
//!
//! Every `SpinMutex` carries a compile-time lock ordering level via
//! `const LEVEL: u8`.  This serves two purposes:
//!
//! 1. **Self-documenting types**: `SpinMutex<FdTable, 4>` makes the
//!    lock's position in the hierarchy visible at every declaration and
//!    function signature.
//! 2. **Runtime enforcement** (debug builds): the [`lockdep`] module
//!    checks that locks are acquired in non-decreasing level order and
//!    panics on violations.
//!
//! # Kernel lock ordering hierarchy
//!
//! Acquire outer-first; never invert. Two locks at the same level must
//! not be held simultaneously unless noted.  The canonical detailed
//! reference (with nesting diagrams and full inventory) is in
//! `kernel/docs/LOCK_ORDERING.md`.
//!
//! ```text
//! Level 0 (outermost -- task / syscall context):
//!   TASK_REGISTRY  (SpinMutex)               proc/signal.rs
//!   VIRTIO_BLK     (SpinMutex, via Once)     drivers/virtio_blk.rs
//!
//! Level 1 (per-process VM):
//!   task.vm_map    (SpinMutex<VmMap, 1>)      proc/task.rs
//!     Rule: when both parent and child are needed, acquire parent first.
//!
//! Level 2 (page tables):
//!   task.pmap      (Arc<SpinMutex<Pmap, 2>>)  mm/vm/map/mod.rs
//!     Always after vm_map.  Parent before child if both needed.
//!
//! Level 3 (VM objects):
//!   VmObject       (LeveledRwLock<VmObject, 3>) mm/vm/object.rs
//!     Shadow/child before backing/parent in chain walks.
//!
//! Level 4 (per-task state):
//!   trap_frame, fd_table, cwd,                proc/task.rs
//!   signals.actions, children, parent_waker
//!     Rule: parent before child when copying during fork.
//!
//! Level 5 (filesystem caches):
//!   VNODE_OBJECTS  then  VNODE_LRU             fs/vnode.rs  (always this order)
//!   DENTRY_CACHE                              fs/dentry.rs
//!   MOUNT_TABLE, SYMLINK_TABLE                fs/mount.rs, fs/symlink.rs
//!
//! Level 6 (filesystem I/O):
//!   REQUEST_QUEUE, DELEGATE_WAKER             fs/delegate.rs
//!   ReplyInner::{waker, value}                fs/delegate.rs
//!
//! Level 7 (physical memory -- IRQ-safe):
//!   PER_CPU_MAGAZINES                         mm/allocator/frame_allocator.rs
//!   GLOBAL_BUDDY                              mm/allocator/frame_allocator.rs
//!   HEAP (#[global_allocator])                alloc_early.rs
//!     Rule: magazine always before buddy.
//!     Acquired inside pmap (Level 2) and VmObject (Level 3) paths.
//!
//! Level 8 (per-CPU scheduler -- IRQ-safe):
//!   PerCpu.run_queue                          executor/per_cpu.rs
//!   PerCpu.timer_wheel                        executor/per_cpu.rs
//!
//! Level 9 (leaf locks -- IRQ-safe):
//!   ASID_ALLOC                                mm/pmap/asid.rs
//!   CONSOLE_INPUT                             console.rs
//!   FUTEX_TABLE                               ipc/futex.rs
//!   WAIT_QUEUES                               mm/vm/wait_queue.rs
//!
//! Level 10 (innermost -- can nest inside ANY lock above):
//!   PRINT_LOCK  (raw AtomicBool)              console.rs
//! ```
//!
//! ## IRQ-context rule
//!
//! Any lock that MAY be acquired from an IRQ / trap handler MUST use
//! [`IrqSafeSpinLock`].  The following locks are confirmed IRQ-accessible:
//!
//! - `GLOBAL_BUDDY`, `PER_CPU_MAGAZINES` (frame allocator)
//! - `HEAP` (`#[global_allocator]`, any `alloc::` call)
//! - `PerCpu.run_queue`, `PerCpu.timer_wheel` (scheduler / timer IRQ)
//! - `CONSOLE_INPUT` (UART IRQ pushes bytes)
//! - `PRINT_LOCK` (raw `AtomicBool` with manual IRQ disable)
//!
//! `SpinMutex` and `LeveledRwLock` are ONLY safe for data accessed
//! exclusively from task / syscall context.

use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

/// A simple CAS spinlock **without** IRQ masking.
///
/// See the module-level documentation for safety constraints and the
/// kernel-wide lock ordering hierarchy.
///
/// The `LEVEL` const generic encodes this lock's position in the
/// ordering hierarchy.  In debug builds, the [`lockdep`] module
/// validates ordering at runtime.
pub struct SpinMutex<T, const LEVEL: u8 = 0> {
    locked: AtomicUsize,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send, const LEVEL: u8> Send for SpinMutex<T, LEVEL> {}
unsafe impl<T: Send, const LEVEL: u8> Sync for SpinMutex<T, LEVEL> {}

impl<T, const LEVEL: u8> SpinMutex<T, LEVEL> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicUsize::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinMutexGuard<'_, T, LEVEL> {
        crate::lockdep::on_acquire(LEVEL);
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

pub struct SpinMutexGuard<'a, T, const LEVEL: u8 = 0> {
    lock: &'a SpinMutex<T, LEVEL>,
}

impl<T, const LEVEL: u8> Deref for SpinMutexGuard<'_, T, LEVEL> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T, const LEVEL: u8> DerefMut for SpinMutexGuard<'_, T, LEVEL> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T, const LEVEL: u8> Drop for SpinMutexGuard<'_, T, LEVEL> {
    fn drop(&mut self) {
        self.lock.locked.store(0, Ordering::Release);
        crate::lockdep::on_release(LEVEL);
    }
}
