//! Async executor core.
//!
//! Per-CPU executor loop: pop Runnable from run queue, poll it, idle with wfi
//! when empty. This is the only place futures get polled.

pub mod per_cpu;
pub mod schedule;

pub use per_cpu::{current, get, init_per_cpu, MAX_CPUS};
pub use schedule::{sleep, spawn_kernel_task, yield_now};

/// The executor loop. Runs forever on the current CPU.
///
/// Pops Runnables from the current CPU's run queue and runs them.
/// When the queue is empty, idles with wfi using the correct SIE
/// enable/disable sequence per CONTEXT.md:
///
///   unlock queue (IRQs restored) -> check empty -> if empty:
///   csrsi sstatus, SIE; wfi; csrci sstatus, SIE -> re-check
pub fn executor_loop() -> ! {
    let pc = per_cpu::current();

    loop {
        // Try to pop a runnable
        if let Some(runnable) = pc.run_queue.pop() {
            runnable.run();
        } else {
            // Queue is empty -- idle with wfi.
            // Critical: enable SIE before wfi so IRQs (timer, IPI) can wake us.
            // IrqSafeSpinLock restores IRQ state on drop, so IRQs are already
            // restored here. We explicitly enable SIE for the wfi window.
            unsafe {
                core::arch::asm!(
                    "csrsi sstatus, 0x2",  // enable SIE
                    "wfi",                  // wait for interrupt
                    "csrci sstatus, 0x2",  // disable SIE
                );
            }
        }
    }
}
