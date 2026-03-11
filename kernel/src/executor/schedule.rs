//! async-task integration: spawn, sleep, yield_now.
//!
//! spawn_kernel_task uses async-task's spawn + schedule_fn to push
//! Runnables into the target CPU's run queue.

use super::per_cpu;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Spawn a kernel task on the target CPU's run queue.
///
/// The schedule_fn closure pushes the Runnable to the target CPU's
/// run queue whenever the future is woken.
pub fn spawn_kernel_task<F>(future: F, target_cpu: usize) -> async_task::Task<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let schedule_fn = move |runnable: async_task::Runnable| {
        per_cpu::get(target_cpu).run_queue.push(runnable);
        // Send IPI if scheduling to a different CPU to wake it from wfi
        let current_cpu = per_cpu::current().cpu_id;
        if target_cpu != current_cpu {
            crate::hal::send_ipi(target_cpu);
        }
    };
    let (runnable, task) = async_task::spawn(future, schedule_fn);
    runnable.schedule();
    task
}

/// A future that yields once (reschedules), then completes.
/// Used for cooperative preemption at trap boundaries.
/// Also checks the per-CPU needs_reschedule flag set by timer IRQ.
pub async fn yield_now() {
    struct YieldFuture {
        yielded: bool,
    }

    impl Future for YieldFuture {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.yielded {
                // Clear the preemption flag
                per_cpu::current()
                    .needs_reschedule
                    .store(false, core::sync::atomic::Ordering::Release);
                Poll::Ready(())
            } else {
                self.yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    YieldFuture { yielded: false }.await
}

/// A future that resolves after the timer wheel expires the entry.
///
/// On first poll, inserts into current CPU's timer_wheel with the waker.
/// On subsequent polls, checks if the timer has expired (waker was called).
///
/// Full timer integration happens in Plan 04 when timer IRQ calls
/// timer_wheel.advance(). For now the API exists and is wired up.
pub async fn sleep(ms: u64) {
    struct SleepFuture {
        ms: u64,
        timer_id: Option<u64>,
    }

    impl Future for SleepFuture {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if let Some(_id) = self.timer_id {
                // We were woken -- timer expired
                crate::klog!(sched, debug, "sleep woke id={} ms={}", _id, self.ms);
                Poll::Ready(())
            } else {
                // First poll: register in timer wheel
                let pc = per_cpu::current();
                let mut tw = pc.timer_wheel.lock();
                let id = tw.insert(self.ms, cx.waker().clone());
                self.timer_id = Some(id);
                crate::klog!(
                    sched,
                    debug,
                    "sleep reg id={} ms={} tick={} cpu={}",
                    id,
                    self.ms,
                    tw.current_tick(),
                    pc.cpu_id
                );
                Poll::Pending
            }
        }
    }

    SleepFuture { ms, timer_id: None }.await
}
