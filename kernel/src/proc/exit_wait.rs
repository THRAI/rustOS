//! sys_exit and wait4 with Waker-based lost-wakeup prevention.

use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::Ordering;
use core::task::{Context, Poll};

use super::task::{Task, TaskState};
use super::syscall_result::SyscallResult;

/// Exit the current process: store exit status, transition to ZOMBIE, wake parent.
pub fn sys_exit(task: &Arc<Task>, code: i32) -> SyscallResult {
    // Store exit status with Release ordering
    task.exit_status.store(code, Ordering::Release);
    // Transition to ZOMBIE with Release ordering
    task.set_zombie();

    // Unregister from global task registry
    super::signal::unregister_task(task.pid);

    // Wake parent's WaitChildFuture if registered, and post SIGCHLD
    if let Some(parent) = task.parent.upgrade() {
        // Post SIGCHLD to parent
        parent.signals.post_signal(super::signal::SIGCHLD);

        let waker = parent.parent_waker.lock().take();
        if let Some(w) = waker {
            w.wake();
        }
        // Also wake parent's top-level waker for signal delivery
        let tlw = parent.top_level_waker.lock().take();
        if let Some(w) = tlw {
            w.wake();
        }
    }

    SyscallResult::Terminated
}

// ---------------------------------------------------------------------------
// WaitChildFuture
// ---------------------------------------------------------------------------

/// Future that waits for a child to become ZOMBIE.
/// Registers Waker FIRST, then scans — no lost wakeup gap.
pub struct WaitChildFuture {
    parent: Arc<Task>,
}

impl WaitChildFuture {
    pub fn new(parent: Arc<Task>) -> Self {
        Self { parent }
    }
}

impl Future for WaitChildFuture {
    /// Returns (child_pid, exit_status) on success.
    type Output = Option<(u32, i32)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Step 1: Register waker FIRST (before scanning) to prevent lost wakeups
        {
            let mut waker_slot = self.parent.parent_waker.lock();
            *waker_slot = Some(cx.waker().clone());
        }

        // Step 2: Scan children for any ZOMBIE (check zombies BEFORE signal guard)
        let children = self.parent.children.lock();
        for child in children.iter() {
            if child.state() == TaskState::Zombie {
                let pid = child.pid;
                let status = child.exit_status.load(Ordering::Acquire);
                drop(children);

                // Remove the zombie child from parent's children list
                self.parent.children.lock().retain(|c| c.pid != pid);

                // Clear waker since we're done
                *self.parent.parent_waker.lock() = None;

                return Poll::Ready(Some((pid, status)));
            }
        }
        drop(children);

        // Step 3: EINTR guard — only after confirming no zombie is ready
        if self.parent.signals.has_actionable_pending() {
            *self.parent.parent_waker.lock() = None;
            return Poll::Ready(None);
        }

        // No zombie children yet — waker is already registered, suspend
        Poll::Pending
    }
}
