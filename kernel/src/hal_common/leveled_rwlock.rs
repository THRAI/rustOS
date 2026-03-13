//! Lockdep-instrumented RwLock wrapper around [`spin::RwLock`].
//!
//! `LeveledRwLock<T, LEVEL>` is a thin wrapper that adds runtime lock
//! ordering validation (in debug builds) to `spin::RwLock`.  It is used
//! for `VmObject` at Level 3 in the kernel lock ordering hierarchy.
//!
//! # Why a wrapper?
//!
//! `spin::RwLock` is a third-party crate — we cannot modify its `read()`
//! and `write()` methods to insert lockdep hooks.  This wrapper delegates
//! all operations and injects `lockdep::on_acquire` / `lockdep::on_release`
//! in the lock/guard lifecycle.
//!
//! # Guard types
//!
//! The wrapper provides its own guard types ([`LeveledRwReadGuard`] and
//! [`LeveledRwWriteGuard`]) that delegate `Deref`/`DerefMut` to spin's
//! guards and call `lockdep::on_release(LEVEL)` on `Drop`.  Since no code
//! in the kernel names `spin::RwLockReadGuard` or `spin::RwLockWriteGuard`
//! explicitly (all bindings are type-inferred via `let`), the substitution
//! is transparent to all call sites.

use core::ops::{Deref, DerefMut};

/// A read-write lock with compile-time level annotation and runtime
/// lock ordering validation.
///
/// Wraps `spin::RwLock<T>` and injects [`lockdep`] hooks.
pub struct LeveledRwLock<T, const LEVEL: u8> {
    inner: spin::RwLock<T>,
}

// SAFETY: Same conditions as spin::RwLock — T must be Send + Sync
// for the lock itself to be Send + Sync.
unsafe impl<T: Send + Sync, const LEVEL: u8> Send for LeveledRwLock<T, LEVEL> {}
unsafe impl<T: Send + Sync, const LEVEL: u8> Sync for LeveledRwLock<T, LEVEL> {}

impl<T, const LEVEL: u8> LeveledRwLock<T, LEVEL> {
    /// Create a new `LeveledRwLock` wrapping `data`.
    ///
    /// Note: `spin::RwLock::new()` is not `const fn` in spin 0.10.0,
    /// so this constructor is also not `const fn`.  All VmObject locks
    /// are constructed inside function bodies, never in `static` items.
    pub fn new(data: T) -> Self {
        Self {
            inner: spin::RwLock::new(data),
        }
    }

    /// Acquire a shared (read) lock.
    ///
    /// In debug builds, calls `lockdep::on_acquire(LEVEL)` before
    /// acquiring the inner lock.
    #[inline]
    pub fn read(&self) -> LeveledRwReadGuard<'_, T, LEVEL> {
        crate::lockdep::on_acquire(LEVEL);
        LeveledRwReadGuard {
            inner: self.inner.read(),
        }
    }

    /// Acquire an exclusive (write) lock.
    ///
    /// In debug builds, calls `lockdep::on_acquire(LEVEL)` before
    /// acquiring the inner lock.
    #[inline]
    pub fn write(&self) -> LeveledRwWriteGuard<'_, T, LEVEL> {
        crate::lockdep::on_acquire(LEVEL);
        LeveledRwWriteGuard {
            inner: self.inner.write(),
        }
    }

    /// Consume the lock and return the inner data.
    ///
    /// No lockdep hooks — this consumes the lock itself (not an
    /// acquire/release cycle on a guard).
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

/// RAII read guard for [`LeveledRwLock`].
///
/// Calls `lockdep::on_release(LEVEL)` when dropped.
pub struct LeveledRwReadGuard<'a, T, const LEVEL: u8> {
    inner: spin::RwLockReadGuard<'a, T>,
}

impl<T, const LEVEL: u8> Deref for LeveledRwReadGuard<'_, T, LEVEL> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T, const LEVEL: u8> Drop for LeveledRwReadGuard<'_, T, LEVEL> {
    #[inline]
    fn drop(&mut self) {
        // Release lockdep tracking.  The inner spin guard is dropped
        // immediately after this, which releases the actual lock.
        crate::lockdep::on_release(LEVEL);
    }
}

/// RAII write guard for [`LeveledRwLock`].
///
/// Calls `lockdep::on_release(LEVEL)` when dropped.
pub struct LeveledRwWriteGuard<'a, T, const LEVEL: u8> {
    inner: spin::RwLockWriteGuard<'a, T>,
}

impl<T, const LEVEL: u8> Deref for LeveledRwWriteGuard<'_, T, LEVEL> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T, const LEVEL: u8> DerefMut for LeveledRwWriteGuard<'_, T, LEVEL> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T, const LEVEL: u8> Drop for LeveledRwWriteGuard<'_, T, LEVEL> {
    #[inline]
    fn drop(&mut self) {
        crate::lockdep::on_release(LEVEL);
    }
}

impl<T: core::fmt::Debug, const LEVEL: u8> core::fmt::Debug for LeveledRwLock<T, LEVEL> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "LeveledRwLock<L{}>", LEVEL)
    }
}
