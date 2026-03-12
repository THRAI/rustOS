//! Async executor core.
//!
//! Per-CPU executor loop: pop Runnable from run queue, poll it, idle with wfi
//! when empty. This is the only place futures get polled.

pub mod per_cpu;
pub mod schedule;
pub mod user_task;

pub use per_cpu::{current, init_per_cpu, set_tp, PerCpu, MAX_CPUS};
pub use schedule::{sleep, spawn_kernel_task, yield_now};
pub use user_task::spawn_user_task;

/// The executor loop. Runs forever on the current CPU.
///
/// Pops Runnables from the current CPU's run queue and runs them.
/// When the queue is empty, idles with wfi.
///
/// IRQ discipline: SIE must be enabled while running tasks so that
/// timer IRQs can fire and advance the per-CPU timer wheel (waking
/// sleep futures). The wfi path disables SIE after wakeup to safely
/// re-check the queue, so we explicitly re-enable SIE before running
/// each task.
pub fn executor_loop() -> ! {
    let pc = per_cpu::current();
    let mut irq_log_count: u32 = 0;

    loop {
        // Try to pop a runnable
        if let Some(runnable) = pc.run_queue.pop() {
            // Ensure IRQs are enabled so timer ticks advance the timer wheel
            // while tasks run. Without this, the wfi path leaves SIE=0 and
            // IrqSafeSpinLock save/restore preserves that disabled state.
            crate::hal::enable();
            if irq_log_count < 5 {
                let sie = crate::hal::is_enabled();
                crate::klog!(sched, debug, "cpu={} run task, SIE={}", pc.cpu_id, sie);
                irq_log_count += 1;
            }
            runnable.run();
        } else {
            // Queue is empty -- idle with wfi.
            // Enable SIE so IRQs (timer, IPI) can wake us, then disable
            // after wakeup to safely loop back and re-check the queue.
            unsafe {
                core::arch::asm!(
                    "csrsi sstatus, 0x2", // enable SIE
                    "wfi",                // wait for interrupt
                    "csrci sstatus, 0x2", // disable SIE
                );
            }
        }
    }
}
