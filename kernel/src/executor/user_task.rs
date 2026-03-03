//! User task future: persistent per-process future + async trap handler.
//!
//! UserTaskFuture wraps run_tasks and switches address space on poll.
//! run_tasks is the trap loop: trap_return → user runs → trap back → dispatch.
//! spawn_user_task creates and detaches the future on a target CPU.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use hal_common::{VirtAddr, PAGE_SIZE};

use crate::hal::rv64::user_trap::trap_return;
use crate::mm::vm::fault::PageFaultAccessType;
use crate::mm::vm::fault_async::resolve_user_fault;
use crate::proc::task::Task;
use crate::proc::user_copy::do_exit;

use super::schedule::{spawn_kernel_task, yield_now};

// Interrupt bit in scause (bit 63 on rv64)
const SCAUSE_INTERRUPT: usize = 1 << 63;

// Interrupt cause codes
const IRQ_S_TIMER: usize = 5;
const IRQ_S_SOFTWARE: usize = 1;
const IRQ_S_EXTERNAL: usize = 9;

// Exception cause codes
const EXC_ECALL_U: usize = 8;
const EXC_INST_PAGE_FAULT: usize = 12;
const EXC_LOAD_PAGE_FAULT: usize = 13;
const EXC_LOAD_ACCESS_FAULT: usize = 5;
const EXC_STORE_ACCESS_FAULT: usize = 7;
const EXC_INST_ACCESS_FAULT: usize = 1;
const EXC_STORE_PAGE_FAULT: usize = 15;

/// Result of handling a user trap.
enum TrapResult {
    /// Continue the trap loop (return to user).
    Continue,
    /// Process has exited.
    Exit,
}

/// The persistent trap loop for a user process.
async fn run_tasks(task: Arc<Task>) {
    klog!(sched, debug, "run_tasks: starting pid={}", task.pid);

    // Capture top-level waker for async signal injection.
    SignalWakeHelper(&task).await;

    loop {
        // Activate per-process page table before signal delivery and returning to user mode.
        // sendsig writes the signal frame to the user stack via copy_user_chunk,
        // which requires the task's pmap to be active.
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_activate(&mut pmap);
        }

        // Check for pending signals before returning to user mode.
        match crate::proc::signal::check_pending_signals(&task) {
            Ok(_) => {} // signal delivered (or none pending), continue
            Err(sig) => {
                // fatal signal (SIGKILL or unhandled)
                // Linux wstatus for signal-killed: low 7 bits = signal number
                let wstatus = crate::proc::exit_wait::WaitStatus::signaled(sig);
                klog!(
                    signal,
                    trace,
                    "run_tasks: FATAL signal pid={} sig={} wstatus={:#x}",
                    task.pid,
                    crate::proc::signal::Signal::new_unchecked(sig),
                    wstatus.0
                );
                do_exit(&task, wstatus);
                break;
            }
        }

        klog!(
            sched,
            debug,
            "run_tasks: calling trap_return pid={}",
            task.pid
        );

        // DEBUG: dump trap frame a0 and wstatus before returning to userspace
        klog!(
            proc,
            trace,
            "PRE-SRET pid={} a0={:#x} sepc={:#x} sp={:#x} ra={:#x}",
            task.pid,
            task.trap_frame.lock().arg(0),
            task.trap_frame.lock().sepc,
            task.trap_frame.lock().sp(),
            task.trap_frame.lock().ra()
        );

        // Return to userspace. Blocks until user traps back.
        trap_return(&task);

        klog!(
            sched,
            debug,
            "run_tasks: returned from trap_return pid={}",
            task.pid
        );

        // Dispatch based on scause in the trap frame.
        // Keep user pmap active — syscall handlers need it for copy_user / fault resolution.
        let result = user_trap_handler(&task).await;

        // Deactivate pmap (back in kernel context).
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_deactivate(&mut pmap);
        }

        match result {
            TrapResult::Continue => {}
            TrapResult::Exit => break,
        }

        // Cooperative preemption point.
        yield_now().await;
    }
}

/// Helper future that captures the executor waker into task.top_level_waker
/// on first poll, then immediately returns Ready.
struct SignalWakeHelper<'a>(&'a Task);

impl<'a> Future for SignalWakeHelper<'a> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        *self.0.top_level_waker.lock() = Some(cx.waker().clone());
        Poll::Ready(())
    }
}

/// Async trap handler: dispatches syscalls, page faults, and interrupts.
async fn user_trap_handler(task: &Arc<Task>) -> TrapResult {
    let scause;
    let stval;
    let sepc;
    {
        let tf = task.trap_frame.lock();
        scause = tf.scause;
        stval = tf.stval;
        sepc = tf.sepc;
    }

    let is_interrupt = scause & SCAUSE_INTERRUPT != 0;
    let code = scause & !SCAUSE_INTERRUPT;

    // Debug: log all traps for pid=1 after verbose flag is set
    klog!(
        trap,
        trace,
        "VERBOSE pid={} trap code={} sepc={:#x} stval={:#x}",
        task.pid,
        code,
        sepc,
        stval
    );

    if is_interrupt {
        match code {
            IRQ_S_TIMER => {
                crate::hal::rv64::timer::handle_timer_irq();
            }
            IRQ_S_SOFTWARE => {
                crate::hal::rv64::ipi::handle_ipi();
            }
            IRQ_S_EXTERNAL => {
                // External IRQ handling
            }
            _ => {
                klog!(trap, debug, "unhandled interrupt: code={}", code);
            }
        }
        return TrapResult::Continue;
    }

    // Exception handling
    match code {
        EXC_ECALL_U => {
            // Syscall handling via unified syscall layer
            dispatch_syscall(task).await
        }
        EXC_LOAD_ACCESS_FAULT
        | EXC_STORE_ACCESS_FAULT
        | EXC_INST_ACCESS_FAULT
        | EXC_INST_PAGE_FAULT
        | EXC_LOAD_PAGE_FAULT
        | EXC_STORE_PAGE_FAULT => {
            let fault_va = VirtAddr::new(stval & !(PAGE_SIZE - 1));
            let access_type = match code {
                EXC_STORE_PAGE_FAULT | EXC_STORE_ACCESS_FAULT => PageFaultAccessType::WRITE,
                EXC_INST_PAGE_FAULT | EXC_INST_ACCESS_FAULT => PageFaultAccessType::EXECUTE,
                _ => PageFaultAccessType::READ,
            };

            // Try to resolve the fault first (demand paging). pcb_onfault is
            // only a fallback — if the page can be mapped, the CPU retries the
            // faulting instruction and the copy continues transparently.
            match resolve_user_fault(task, fault_va, access_type).await {
                Ok(_) => {
                    // Page resolved — return to the faulting instruction.
                    // pcb_onfault (if set) stays armed for future faults.
                    TrapResult::Continue
                }
                Err(e) => {
                    // Resolution failed. If pcb_onfault is set (copy_user_chunk),
                    // redirect to the EFAULT landing pad instead of killing.
                    let percpu = crate::executor::per_cpu::current();
                    let onfault = percpu
                        .pcb_onfault
                        .load(core::sync::atomic::Ordering::Relaxed);
                    if onfault != 0 {
                        task.trap_frame.lock().sepc = onfault;
                        percpu
                            .pcb_onfault
                            .store(0, core::sync::atomic::Ordering::Relaxed);
                        return TrapResult::Continue;
                    }
                    // No fixup — truly fatal user fault.
                    let pc = task.trap_frame.lock().sepc;
                    klog!(
                        trap,
                        trace,
                        "fatal fault: pid={} va={:#x} pc={:#x} code={} err={:?}",
                        task.pid,
                        stval,
                        pc,
                        code,
                        e
                    );
                    task.signals.post_signal(crate::proc::signal::SIGSEGV);
                    TrapResult::Continue
                }
            }
        }
        _ => {
            klog!(
                trap,
                trace,
                "unhandled exception: code={} sepc={:#x} stval={:#x}",
                code,
                { task.trap_frame.lock().sepc },
                stval
            );
            task.signals.post_signal(crate::proc::signal::SIGSEGV);
            TrapResult::Continue
        }
    }
}

/// Syscall trap path routed through the unified syscall layer.
async fn dispatch_syscall(task: &Arc<Task>) -> TrapResult {
    let (syscall_id, args) = {
        let tf = task.trap_frame.lock();
        (tf.x[17], [tf.x[10], tf.x[11], tf.x[12], tf.x[13], tf.x[14], tf.x[15]])
    };

    match crate::syscall::syscall(task, syscall_id, args).await {
        crate::syscall::SyscallAction::Return(ret) => {
            let mut tf = task.trap_frame.lock();
            tf.advance_pc();
            tf.set_ret_val(ret);
            TrapResult::Continue
        }
        crate::syscall::SyscallAction::Continue => TrapResult::Continue,
        crate::syscall::SyscallAction::Exit => TrapResult::Exit,
    }
}

/// Spawn a user task on the specified CPU.
pub fn spawn_user_task(task: Arc<Task>, cpu: usize) {
    spawn_kernel_task(UserTaskFuture { task }, cpu);
}

/// Wrapper future that activates the task's page table on poll.
struct UserTaskFuture {
    task: Arc<Task>,
}

impl Future for UserTaskFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // Activate the task's page table before running.
        let mut pmap = self.task.pmap.lock();
        crate::mm::pmap::pmap_activate(&mut pmap);
        drop(pmap);

        // Poll the actual task loop.
        let task = self.task.clone();
        let mut fut = Box::pin(run_tasks(task));
        let result = fut.as_mut().poll(cx);

        // Deactivate the page table after running.
        let mut pmap = self.task.pmap.lock();
        crate::mm::pmap::pmap_deactivate(&mut pmap);

        result
    }
}
