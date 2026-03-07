//! Futex: fast user-space mutual exclusion.
//!
//! Process-private futexes keyed by physical address.
//! Global HashMap<PhysAddr, Vec<Waker>> for wait/wake.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::hal_common::{Errno, PhysAddr};

use crate::hal_common::IrqSafeSpinLock;

use crate::proc::task::Task;

/// Global futex wait table: maps physical address to list of waiting wakers.
static FUTEX_TABLE: IrqSafeSpinLock<BTreeMap<usize, Vec<Waker>>> =
    IrqSafeSpinLock::new(BTreeMap::new());

/// Park the current task on a futex key (physical address).
/// Returns a future that completes when woken by futex_wake or interrupted by signal.
pub async fn futex_wait(pa_key: PhysAddr, task: &Arc<Task>) -> Result<(), Errno> {
    klog!(proc, debug, "futex_wait pid={} key={:#x}", task.pid, pa_key.as_usize());
    FutexWaitFuture { pa_key: pa_key.as_usize(), registered: false, task }.await
}

/// Wake up to `count` waiters on the given futex key.
/// Returns the number of waiters actually woken.
pub fn futex_wake(pa_key: PhysAddr, count: usize) -> usize {
    let key = pa_key.as_usize();
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
    pa_key: usize,
    registered: bool,
    task: &'a Arc<Task>,
}

impl<'a> Future for FutexWaitFuture<'a> {
    type Output = Result<(), Errno>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // EINTR guard: check for pending signals before blocking
        if self.task.signals.has_actionable_pending() {
            return Poll::Ready(Err(Errno::EINTR));
        }
        if self.registered {
            // We were woken
            Poll::Ready(Ok(()))
        } else {
            // First poll: register waker in the futex table
            let mut table = FUTEX_TABLE.lock();
            table.entry(self.pa_key).or_insert_with(Vec::new).push(cx.waker().clone());
            self.registered = true;
            Poll::Pending
        }
    }
}
