//! Futex: fast user-space mutual exclusion.
//!
//! Process-private futexes keyed by physical address.
//! Global HashMap<PhysAddr, Vec<Waker>> for wait/wake.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use crate::{
    hal_common::{Errno, IrqSafeSpinLock, PhysAddr},
    proc::Task,
};

/// Global futex wait table: maps physical address to list of waiting wakers.
///
/// Lock ordering: **Level 9** (leaf lock).  Accessed only from syscall
/// context (`futex_wait`, `futex_wake`).  Never nests inside or outside
/// any other lock.  IRQ-safe by type, but not accessed from IRQ handlers.
static FUTEX_TABLE: IrqSafeSpinLock<BTreeMap<PhysAddr, Vec<(u32, Waker)>>, 9> =
    IrqSafeSpinLock::new(BTreeMap::new());

/// Park the current task on a futex key (physical address).
/// `deadline_ms` = 0 means no timeout; otherwise returns Err(Etimedout) after that time.
pub async fn futex_wait(pa_key: PhysAddr, task: &Arc<Task>, deadline_ms: u64) -> Result<(), Errno> {
    klog!(proc, debug, "futex_wait pid={} key={:#x}", task.pid, pa_key);
    FutexWaitFuture {
        pa_key,
        registered: false,
        task,
        deadline_ms,
    }
    .await
}

/// Wake up to `count` waiters on the given futex key.
/// Returns the number of waiters actually woken.
pub fn futex_wake(pa_key: PhysAddr, count: usize) -> usize {
    let key = pa_key;
    let mut table = FUTEX_TABLE.lock();
    let waiters = match table.get_mut(&key) {
        Some(v) => v,
        None => return 0,
    };

    let to_wake = core::cmp::min(count, waiters.len());
    let mut woken = 0;
    for _ in 0..to_wake {
        if let Some((_pid, waker)) = waiters.pop() {
            waker.wake();
            woken += 1;
        }
    }

    // Clean up empty entries
    if waiters.is_empty() {
        table.remove(&key);
    }

    klog!(proc, debug, "futex_wake key={:#x} woken={}", key, woken);
    woken
}

/// Future that parks on a futex key until woken.
struct FutexWaitFuture<'a> {
    pa_key: PhysAddr,
    registered: bool,
    task: &'a Arc<Task>,
    /// Absolute deadline in ms (0 = no timeout).
    deadline_ms: u64,
}

impl<'a> Future for FutexWaitFuture<'a> {
    type Output = Result<(), Errno>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Timeout check: if deadline passed, remove from table and return ETIMEDOUT.
        if self.deadline_ms > 0 {
            let now = crate::hal::read_time_ms();
            if now >= self.deadline_ms {
                if self.registered {
                    let mut table = FUTEX_TABLE.lock();
                    if let Some(waiters) = table.get_mut(&self.pa_key) {
                        waiters.retain(|(pid, _)| *pid != self.task.pid);
                        if waiters.is_empty() {
                            table.remove(&self.pa_key);
                        }
                    }
                }
                return Poll::Ready(Err(Errno::Etimedout));
            }
        }

        // EINTR guard: pending signal interrupts futex wait.
        if self.task.signals.has_actionable_pending() {
            if self.registered {
                let mut table = FUTEX_TABLE.lock();
                if let Some(waiters) = table.get_mut(&self.pa_key) {
                    waiters.retain(|(pid, _)| *pid != self.task.pid);
                    if waiters.is_empty() {
                        table.remove(&self.pa_key);
                    }
                }
            }
            return Poll::Ready(Err(Errno::Eintr));
        }

        if !self.registered {
            // First poll: register waker in the futex table.
            let mut table = FUTEX_TABLE.lock();
            table
                .entry(self.pa_key)
                .or_default()
                .push((self.task.pid, cx.waker().clone()));
            self.registered = true;
            return Poll::Pending;
        }

        // Already registered: return Ready only if we were actually removed
        // by futex_wake(). Otherwise keep waiting.
        let mut table = FUTEX_TABLE.lock();
        if let Some(waiters) = table.get_mut(&self.pa_key) {
            if waiters.iter().any(|(pid, _)| *pid == self.task.pid) {
                // Still queued; refresh waker for future wakeups.
                for (pid, w) in waiters.iter_mut() {
                    if *pid == self.task.pid {
                        *w = cx.waker().clone();
                        break;
                    }
                }
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        } else {
            // Key vanished -> this waiter has been woken.
            Poll::Ready(Ok(()))
        }
    }
}
