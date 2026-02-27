//! sys_exit and wait4 with Waker-based lost-wakeup prevention.

use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::Ordering;
use core::task::{Context, Poll};

use super::task::{Task, TaskState};
use super::syscall_result::SyscallResult;

/// POSIX wait status wrapper (compatible with Linux waitpid wstatus bitfield).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitStatus(pub i32);

impl WaitStatus {
    /// Create a wait status for a process that exited normally.
    /// Format: `(code & 0xff) << 8`
    #[inline]
    pub const fn exited(code: i32) -> Self {
        Self((code & 0xff) << 8)
    }

    /// Create a wait status for a process terminated by a signal.
    /// Format: `sig & 0x7f`
    #[inline]
    pub const fn signaled(sig: u8) -> Self {
        Self((sig & 0x7f) as i32)
    }
}

/// Exit the current process: store exit status, transition to ZOMBIE, wake parent.
pub fn sys_exit(task: &Arc<Task>, status: WaitStatus) -> SyscallResult {
    klog!(proc, debug, "exit pid={} wstatus={:#x}", task.pid, status.0);
    // Store exit status with Release ordering
    task.exit_status.store(status.0, Ordering::Release);
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
    target_pid: isize,
}

impl WaitChildFuture {
    /// target_pid:
    ///   > 0  => wait for specific child
    ///   -1   => wait for any child
    pub fn new(parent: Arc<Task>, target_pid: isize) -> Self {
        Self { parent, target_pid }
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

        // Step 2: If no children at all, return None (ECHILD)
        let children = self.parent.children.lock();
        if children.is_empty() {
            *self.parent.parent_waker.lock() = None;
            return Poll::Ready(None);
        }

        // Step 3: Scan children for any ZOMBIE matching target_pid
        for child in children.iter() {
            if child.state() == TaskState::Zombie {
                if self.target_pid > 0 && child.pid != self.target_pid as u32 {
                    continue;
                }

                let pid = child.pid;
                let status = child.exit_status.load(Ordering::Acquire);
                drop(children);

                // Remove the zombie child from parent's children list
                self.parent.children.lock().retain(|c| c.pid != pid);

                klog!(proc, debug, "wait4 reaped pid={} status={}", pid, status);

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
