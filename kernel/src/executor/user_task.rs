//! User task future: persistent per-process future + async trap handler.
//!
//! UserTaskFuture wraps run_tasks and switches address space on poll.
//! run_tasks is the trap loop: trap_return → user runs → trap back → dispatch.
//! spawn_user_task creates and detaches the future on a target CPU.

use alloc::{boxed::Box, sync::Arc};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::{
    executor::{spawn_kernel_task, yield_now},
    hal::trap_return,
    hal_common::{VirtAddr, PAGE_SIZE},
    mm::{resolve_user_fault, PageFaultAccessType},
    proc::{check_pending_signals, do_exit, Task, WaitStatus},
};

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

    loop {
        // Capture top-level waker for async signal injection at the start of each iteration.
        // This ensures kill() → post_signal() → top_level_waker.wake() can re-enqueue
        // the task even if it's parked in a blocking syscall.
        SignalWakeHelper(&task).await;

        // Activate per-process page table before signal delivery and returning to user mode.
        // sendsig writes the signal frame to the user stack via copy_user_chunk,
        // which requires the task's pmap to be active.
        {
            let vm_map = task.vm_map.read();
            let mut pmap = vm_map.pmap_lock();
            crate::hal::activate_pmap(&mut pmap);
        }

        // Check for pending signals before returning to user mode.
        match check_pending_signals(&task) {
            Ok(_) => {}, // signal delivered (or none pending), continue
            Err(sig) => {
                // fatal signal (SIGKILL or unhandled)
                // Linux wstatus for signal-killed: low 7 bits = signal number
                let wstatus = WaitStatus::signaled(sig);
                klog!(
                    signal,
                    trace,
                    "run_tasks: FATAL signal pid={} sig={} wstatus={:#x}",
                    task.pid,
                    crate::proc::Signal::new_unchecked(sig),
                    wstatus.0
                );
                do_exit(&task, wstatus);
                break;
            },
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
            task.trap_frame.lock().pc(),
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
            let vm_map = task.vm_map.read();
            let mut pmap = vm_map.pmap_lock();
            crate::hal::deactivate_pmap(&mut pmap);
        }

        match result {
            TrapResult::Continue => {},
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
    let info = {
        let tf = task.trap_frame.lock();
        crate::hal::trap::describe(&tf)
    };

    // Debug: log all traps for pid=1 after verbose flag is set
    klog!(
        trap,
        trace,
        "VERBOSE pid={} trap code={} sepc={:#x} stval={:#x}",
        task.pid,
        match info.cause {
            crate::hal::trap::TrapCause::Unknown(code) => code,
            _ => 0,
        },
        info.pc,
        info.fault_addr
    );

    match info.cause {
        crate::hal::trap::TrapCause::Timer => {
            crate::hal::handle_timer_irq();
            TrapResult::Continue
        },
        crate::hal::trap::TrapCause::Software => {
            crate::hal::handle_ipi();
            TrapResult::Continue
        },
        crate::hal::trap::TrapCause::External => {
            // External IRQ handling
            TrapResult::Continue
        },
        crate::hal::trap::TrapCause::Syscall => {
            // Syscall handling via unified syscall layer
            dispatch_syscall(task).await
        },
        crate::hal::trap::TrapCause::AccessFaultRead
        | crate::hal::trap::TrapCause::PageFaultRead
        | crate::hal::trap::TrapCause::AccessFaultWrite
        | crate::hal::trap::TrapCause::PageFaultWrite
        | crate::hal::trap::TrapCause::AccessFaultExecute
        | crate::hal::trap::TrapCause::PageFaultExecute => {
            let fault_va = VirtAddr::new(info.fault_addr);
            let access_type = match info.cause {
                crate::hal::trap::TrapCause::AccessFaultWrite
                | crate::hal::trap::TrapCause::PageFaultWrite => PageFaultAccessType::WRITE,
                crate::hal::trap::TrapCause::AccessFaultExecute
                | crate::hal::trap::TrapCause::PageFaultExecute => PageFaultAccessType::EXECUTE,
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
                },
                Err(e) => {
                    // No fixup here — kernel-mode copy_user faults are handled in
                    // the HAL trap path via pcb_onfault.
                    let pc = task.trap_frame.lock().pc();
                    klog!(
                        trap,
                        error,
                        "fatal fault: pid={} va={:#x} pc={:#x} code={} err={:?}",
                        task.pid,
                        info.fault_addr,
                        pc,
                        0,
                        e
                    );
                    task.signals.post_signal(crate::proc::SIGSEGV);
                    TrapResult::Continue
                },
            }
        },
        _ => {
            klog!(
                trap,
                error,
                "unhandled exception: code={} sepc={:#x} stval={:#x}",
                0,
                info.pc,
                info.fault_addr
            );
            task.signals.post_signal(crate::proc::SIGSEGV);
            TrapResult::Continue
        },
    }
}

/// Syscall trap path routed through the unified syscall layer.
async fn dispatch_syscall(task: &Arc<Task>) -> TrapResult {
    let (syscall_id, args) = {
        let tf = task.trap_frame.lock();
        (
            crate::hal::syscall_abi::nr(&tf),
            crate::hal::syscall_abi::args(&tf),
        )
    };

    match crate::syscall::syscall(task, syscall_id, args).await {
        crate::syscall::SyscallAction::Return(ret) => {
            let mut tf = task.trap_frame.lock();
            crate::hal::syscall_abi::advance(&mut tf);
            crate::hal::syscall_abi::set_return(&mut tf, ret);
            TrapResult::Continue
        },
        crate::syscall::SyscallAction::Continue => TrapResult::Continue,
        crate::syscall::SyscallAction::Exit => TrapResult::Exit,
    }
}

/// Spawn a user task on the specified CPU.
pub fn spawn_user_task(task: Arc<Task>, cpu: usize) {
    //crate::kprintln!("[DEBUG] spawn_user_task pid={} cpu={}", task.pid, cpu);
    spawn_kernel_task(UserTaskFuture::new(task), cpu).detach();
}

/// Wrapper future that activates the task's page table on poll.
struct UserTaskFuture {
    task: Arc<Task>,
    inner: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl UserTaskFuture {
    fn new(task: Arc<Task>) -> Self {
        let t = task.clone();
        Self {
            task,
            inner: Box::pin(run_tasks(t)),
        }
    }
}

impl Future for UserTaskFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = unsafe { self.get_unchecked_mut() };
        //crate::kprintln!("[DEBUG] UserTaskFuture::poll pid={}", this.task.pid);

        // Activate the task's page table before running.
        {
            let vm_map = this.task.vm_map.read();
            let mut pmap = vm_map.pmap_lock();
            crate::hal::activate_pmap(&mut pmap);
        }

        // Poll the persistent inner future.
        let result = this.inner.as_mut().poll(cx);

        // Deactivate the page table after running.
        {
            let vm_map = this.task.vm_map.read();
            let mut pmap = vm_map.pmap_lock();
            crate::hal::deactivate_pmap(&mut pmap);
        }

        result
    }
}
