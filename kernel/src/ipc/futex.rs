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
static FUTEX_TABLE: IrqSafeSpinLock<BTreeMap<PhysAddr, Vec<Waker>>, 9> =
    IrqSafeSpinLock::new(BTreeMap::new());

/// Park the current task on a futex key (physical address).
/// Returns a future that completes when woken by futex_wake or interrupted by signal.
pub async fn futex_wait(pa_key: PhysAddr, task: &Arc<Task>) -> Result<(), Errno> {
    klog!(proc, debug, "futex_wait pid={} key={:#x}", task.pid, pa_key);
    FutexWaitFuture {
        pa_key,
        registered: false,
        task,
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
        if let Some(waker) = waiters.pop() {
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
}

impl<'a> Future for FutexWaitFuture<'a> {
    type Output = Result<(), Errno>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // EINTR guard: check for pending signals before blocking
        if self.task.signals.has_actionable_pending() {
            return Poll::Ready(Err(Errno::Eintr));
        }
        if self.registered {
            // We were woken
            Poll::Ready(Ok(()))
        } else {
            // First poll: register waker in the futex table
            let mut table = FUTEX_TABLE.lock();
            table
                .entry(self.pa_key)
                .or_default()
                .push(cx.waker().clone());
            self.registered = true;
            Poll::Pending
        }
    }
}
