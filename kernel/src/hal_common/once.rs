//! RISC-V safe Once cell using AtomicUsize (not AtomicBool/AtomicU8).
//!
//! RISC-V only has native atomics for 32-bit and 64-bit words.
//! Sub-word atomics (AtomicBool, AtomicU8) are emulated by LLVM with
//! lr/sc + masking, which can hang on bare metal.

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicUsize, Ordering},
};

const INCOMPLETE: usize = 0;
const RUNNING: usize = 1;
const COMPLETE: usize = 2;

/// A thread-safe cell that can be written to only once.
pub struct Once<T> {
    state: AtomicUsize,
    data: UnsafeCell<Option<T>>,
}

unsafe impl<T: Send + Sync> Send for Once<T> {}
unsafe impl<T: Send + Sync> Sync for Once<T> {}

impl<T> Once<T> {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(INCOMPLETE),
            data: UnsafeCell::new(None),
        }
    }

    /// Initialize the cell with the result of `f`. If already initialized, does nothing.
    pub fn call_once<F: FnOnce() -> T>(&self, f: F) {
        if self.state.load(Ordering::Acquire) == COMPLETE {
            return;
        }
        match self
            .state
            .compare_exchange(INCOMPLETE, RUNNING, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => {
                let val = f();
                unsafe { *self.data.get() = Some(val) };
                self.state.store(COMPLETE, Ordering::Release);
            },
            Err(_) => {
                // Another thread is running or already complete — spin until complete
                while self.state.load(Ordering::Acquire) != COMPLETE {
                    core::hint::spin_loop();
                }
            },
        }
    }

    /// Get a reference to the stored value, or None if not yet initialized.
    pub fn get(&self) -> Option<&T> {
        if self.state.load(Ordering::Acquire) == COMPLETE {
            unsafe { (*self.data.get()).as_ref() }
        } else {
            None
        }
    }
}
