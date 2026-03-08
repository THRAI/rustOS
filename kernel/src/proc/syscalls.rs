//! Process subsystem syscalls.
//!
//! Includes fork/clone, exec, wait4, exit, and signal management endpoints.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::executor::per_cpu;
use crate::executor::user_task::{
    copyin_argv, copyinstr, do_exit, fault_in_user_buffer, spawn_user_task,
};
use crate::klog;
use crate::proc::exit_wait::WaitStatus;
use crate::proc::fork::fork;
use crate::proc::signal::{SigAction, SigFrame, Signal, MAX_SIG, SIGFRAME_SIZE, SIGKILL, SIGSTOP};
use crate::proc::task::Task;
use crate::hal_common::Errno;

// ---------------------------------------------------------------------------
// Basic Info Syscalls
// ---------------------------------------------------------------------------

pub fn sys_getpid(task: &Arc<Task>) -> usize {
    task.pid as usize
}

pub fn sys_getppid(task: &Arc<Task>) -> usize {
    task.ppid() as usize
}

pub fn sys_getuid(_task: &Arc<Task>) -> usize {
    0
}
pub fn sys_geteuid(_task: &Arc<Task>) -> usize {
    0
}
pub fn sys_getgid(_task: &Arc<Task>) -> usize {
    0
}
pub fn sys_getegid(_task: &Arc<Task>) -> usize {
    0
}

pub fn sys_gettid(task: &Arc<Task>) -> usize {
    task.pid as usize
}

// ---------------------------------------------------------------------------
// Process Lifecycle: clone, execve, exit, wait4
// ---------------------------------------------------------------------------

pub fn sys_clone(task: &Arc<Task>) -> usize {
    // Basic fork (flags ignored for now)
    let child = fork(task);
    let child_pid = child.pid;
    // Spawn child on same CPU
    let cpu = per_cpu::current().cpu_id;
    spawn_user_task(child, cpu);
    child_pid as usize
}

pub fn sys_exit(task: &Arc<Task>, status: i32) {
    let wstatus = WaitStatus::exited(status);
    do_exit(task, wstatus);
}

pub async fn sys_execve_async(
    task: &Arc<Task>,
    pathname_ptr: usize,
    argv_ptr: usize,
    envp_ptr: usize,
) -> Result<(usize, usize), Errno> {
    // Read pathname from user memory
    let path = match copyinstr(task, pathname_ptr, 256).await {
        None => return Err(Errno::Efault),
        Some(s) => s,
    };
    // Read argv array from user memory (before exec destroys address space)
    let argv = copyin_argv(task, argv_ptr, 64, 4096).await;
    // Read envp array
    let envp = copyin_argv(task, envp_ptr, 64, 4096).await;

    crate::proc::exec::exec_with_args(task, &path, &argv, &envp).await
}

pub async fn sys_wait4_async(
    task: &Arc<Task>,
    pid: isize,
    wstatus_ptr: usize,
    options: usize,
) -> Result<u32, Errno> {
    const WNOHANG: usize = 1;

    // Pre-fault wstatus page so copy_user_chunk won't EFAULT on demand-paged stack
    if wstatus_ptr != 0 {
        fault_in_user_buffer(
            task,
            wstatus_ptr,
            4,
            crate::mm::vm::fault::PageFaultAccessType::WRITE,
        )
        .await;
    }

    // Check if there are any children at all
    {
        let children = task.children.lock();
        if children.is_empty() {
            return Err(Errno::Echild);
        }
    }

    if options & WNOHANG != 0 {
        // Non-blocking: scan for a zombie child, return immediately
        let children = task.children.lock();
        for child in children.iter() {
            if child.state() == crate::proc::task::TaskState::Zombie {
                if pid > 0 && child.pid != pid as u32 {
                    continue;
                }

                let child_pid = child.pid;
                let status = child
                    .exit_status
                    .load(core::sync::atomic::Ordering::Acquire);
                drop(children);

                // Remove the zombie child from parent's children list
                task.children.lock().retain(|c| c.pid != child_pid);

                // Consume pending SIGCHLD
                let mut sig_chld = crate::proc::signal::SigSet::empty();
                sig_chld.add(Signal::new_unchecked(crate::proc::signal::SIGCHLD));
                task.signals
                    .pending
                    .fetch_difference(sig_chld, core::sync::atomic::Ordering::Release);
                // Write status to user memory if pointer is non-null
                if wstatus_ptr != 0 {
                    klog!(
                        proc,
                        trace,
                        "wait4(WNOHANG) pid={} reaped child={} wstatus={:#x}",
                        task.pid,
                        child_pid,
                        status
                    );
                    let rc = unsafe {
                        crate::hal::rv64::copy_user::copy_user_chunk(
                            wstatus_ptr as *mut u8,
                            &status as *const i32 as *const u8,
                            4,
                        )
                    };
                    if rc != 0 {
                        return Err(Errno::Efault);
                    }
                }
                return Ok(child_pid);
            }
        }
        drop(children);
        // No zombie yet, WNOHANG: return 0 (not an error)
        return Ok(0);
    }

    // Blocking path
    use crate::proc::exit_wait::WaitChildFuture;
    let result = WaitChildFuture::new(Arc::clone(task), pid).await;

    match result {
        Some((child_pid, status)) => {
            // Consume pending SIGCHLD
            let mut sig_chld = crate::proc::signal::SigSet::empty();
            sig_chld.add(Signal::new_unchecked(crate::proc::signal::SIGCHLD));
            task.signals
                .pending
                .fetch_difference(sig_chld, core::sync::atomic::Ordering::Release);
            // Write status to user memory if pointer is non-null
            if wstatus_ptr != 0 {
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        wstatus_ptr as *mut u8,
                        &status as *const i32 as *const u8,
                        4,
                    )
                };
                if rc != 0 {
                    return Err(Errno::Efault);
                }
            }
            Ok(child_pid)
        }
        None => {
            if task.signals.has_actionable_pending() {
                Err(Errno::Eintr)
            } else {
                Err(Errno::Echild)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Signals: sigreturn, sigaction, sigprocmask
// ---------------------------------------------------------------------------

pub fn sys_sigreturn(task: &Arc<Task>) -> Result<(), Errno> {
    klog!(signal, debug, "sigreturn pid={}", task.pid);
    let sp = task.trap_frame.lock().x[2]; // current SP points to sigframe

    // Copyin the sigframe from user memory
    let mut frame = core::mem::MaybeUninit::<SigFrame>::uninit();
    let ok = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            frame.as_mut_ptr() as *mut u8,
            sp as *const u8,
            SIGFRAME_SIZE,
        )
    };
    if ok != 0 {
        return Err(Errno::Efault);
    }
    let frame = unsafe { frame.assume_init() };

    // Validate sepc: must be in user space (< 0x0000_0040_0000_0000)
    const USER_MAX_VA: usize = 0x0000_0040_0000_0000;
    if frame.saved_tf.sepc >= USER_MAX_VA {
        return Err(Errno::Einval);
    }

    // Restore trap frame with sanitization
    {
        let mut tf = task.trap_frame.lock();
        tf.x = frame.saved_tf.x;
        tf.sepc = frame.saved_tf.sepc;
        // Sanitize sstatus: SPP cleared (user mode), SPIE set, FS>=Initial
        tf.sstatus = (frame.saved_tf.sstatus & !(1 << 8)) | (1 << 5);
        if tf.sstatus & (3 << 13) == 0 {
            tf.sstatus |= 1 << 13; // FS=Initial if Off
        }
    }

    // Restore signal mask
    task.signals.blocked.store(
        crate::proc::signal::SigSet::from_u64(frame.saved_mask),
        Ordering::Release,
    );

    Ok(())
}

const SIGACTION_USER_SIZE: usize = 32;

pub fn sys_sigaction(
    task: &Arc<Task>,
    sig: usize,
    act_ptr: usize,
    oldact_ptr: usize,
) -> Result<usize, Errno> {
    if sig < 1 || sig > MAX_SIG as usize {
        return Err(Errno::Einval);
    }
    if sig == SIGKILL as usize || sig == SIGSTOP as usize {
        return Err(Errno::Einval);
    }

    let idx = sig - 1;
    let mut actions = task.signals.actions.lock();

    // Write old action to user memory
    if oldact_ptr != 0 {
        let old = actions[idx];
        let buf: [u64; 4] = [old.handler as u64, old.flags, old.restorer as u64, old.mask];
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                oldact_ptr as *mut u8,
                buf.as_ptr() as *const u8,
                SIGACTION_USER_SIZE,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    // Read new action from user memory
    if act_ptr != 0 {
        let mut buf = [0u64; 4];
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                buf.as_mut_ptr() as *mut u8,
                act_ptr as *const u8,
                SIGACTION_USER_SIZE,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
        actions[idx] = SigAction {
            handler: buf[0] as usize,
            flags: buf[1],
            restorer: buf[2] as usize,
            mask: buf[3],
        };
    }

    Ok(0)
}

pub const SIG_BLOCK: usize = 0;
pub const SIG_UNBLOCK: usize = 1;
pub const SIG_SETMASK: usize = 2;

pub fn sys_sigprocmask(
    task: &Arc<Task>,
    how: usize,
    set_ptr: usize,
    oldset_ptr: usize,
) -> Result<usize, Errno> {
    let sig_state = &task.signals;

    if oldset_ptr != 0 {
        let old = sig_state.blocked.load(Ordering::Relaxed);
        let old_u64 = old.as_u64();
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                oldset_ptr as *mut u8,
                &old_u64 as *const u64 as *const u8,
                8,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    if set_ptr != 0 {
        let mut new_set: u64 = 0;
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                &mut new_set as *mut u64 as *mut u8,
                set_ptr as *const u8,
                8,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }

        let unblockable = crate::proc::signal::SigSet::empty()
            .add(Signal::new_unchecked(SIGKILL))
            .add(Signal::new_unchecked(SIGSTOP))
            .as_u64();

        new_set &= !unblockable;
        let set = crate::proc::signal::SigSet::from_u64(new_set);

        match how {
            SIG_BLOCK => {
                sig_state.blocked.fetch_union(set, Ordering::Release);
            }
            SIG_UNBLOCK => {
                sig_state.blocked.fetch_difference(set, Ordering::Release);
            }
            SIG_SETMASK => {
                sig_state.blocked.store(set, Ordering::Release);
            }
            _ => return Err(Errno::Einval),
        }
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// sys_kill, sys_setpgid, sys_getpgid
// ---------------------------------------------------------------------------

pub fn sys_kill(sender: &Arc<Task>, pid: isize, sig: u8) -> Result<usize, Errno> {
    klog!(
        signal,
        debug,
        "kill pid={} -> target={} sig={}",
        sender.pid,
        pid,
        Signal::new_unchecked(sig)
    );
    if sig > MAX_SIG {
        return Err(Errno::Einval);
    }

    if pid > 0 {
        let target = crate::proc::signal::find_task_by_pid(pid as u32);
        match target {
            Some(t) => {
                if sig > 0 {
                    t.signals.post_signal(sig);
                    if let Some(w) = t.top_level_waker.lock().take() {
                        w.wake();
                    }
                }
                Ok(0)
            }
            None => Err(Errno::Esrch),
        }
    } else if pid == 0 {
        let pgid = sender.pgid.load(Ordering::Relaxed);
        crate::proc::signal::kill_pgrp(pgid, sig);
        Ok(0)
    } else if pid == -1 {
        crate::proc::signal::for_each_task(|t| {
            if t.pid != 1 && sig > 0 {
                t.signals.post_signal(sig);
                if let Some(w) = t.top_level_waker.lock().take() {
                    w.wake();
                }
            }
        });
        Ok(0)
    } else {
        let pgid = (-pid) as u32;
        crate::proc::signal::kill_pgrp(pgid, sig);
        Ok(0)
    }
}

pub fn sys_setpgid(task: &Arc<Task>, pid: u32, pgid: u32) -> Result<usize, Errno> {
    let target_pid = if pid == 0 { task.pid } else { pid };
    let new_pgid = if pgid == 0 { target_pid } else { pgid };

    if target_pid == task.pid {
        task.pgid.store(new_pgid, Ordering::Release);
        Ok(0)
    } else {
        let children = task.children.lock();
        for child in children.iter() {
            if child.pid == target_pid {
                child.pgid.store(new_pgid, Ordering::Release);
                return Ok(0);
            }
        }
        Err(Errno::Esrch)
    }
}

pub fn sys_getpgid(task: &Arc<Task>, pid: u32) -> Result<usize, Errno> {
    if pid == 0 {
        Ok(task.pgid.load(Ordering::Relaxed) as usize)
    } else {
        if let Some(t) = crate::proc::signal::find_task_by_pid(pid) {
            Ok(t.pgid.load(Ordering::Relaxed) as usize)
        } else {
            Err(Errno::Esrch)
        }
    }
}
