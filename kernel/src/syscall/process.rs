//! Process management system calls.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::{
    executor::{current, sleep, spawn_kernel_task, spawn_user_task},
    hal_common::{Errno, KernelResult},
    mm::PageFaultAccessType,
    proc::{
        copyin_argv, copyinstr, do_clone, do_execve, do_exit, fault_in_user_buffer,
        find_task_by_pid, CloneFlags, SigSet, Signal, Task, TaskState,
        WaitChildFuture, WaitStatus, SIGCHLD,
    },
};

// ---------------------------------------------------------------------------
// Basic Info Syscalls
// ---------------------------------------------------------------------------

pub fn sys_getpid(task: &Arc<Task>) -> usize {
    task.tgid as usize
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

pub fn sys_clone(
    task: &Arc<Task>,
    flags: usize,
    child_stack: usize,
    parent_tid: usize,
    child_tid: usize,
    tls: usize,
) -> KernelResult<usize> {
    let clone_flags = CloneFlags::from_bits_truncate(flags as u64);

    let (child, vfork_done) = match do_clone(task, clone_flags, child_stack, tls, child_tid) {
        Ok(v) => v,
        Err(e) => {
            return Err(kerr!(
                proc,
                error,
                e,
                "sys_clone: do_clone failed flags={:#x} stack={:#x} tls={:#x} child_tid={:#x}",
                flags,
                child_stack,
                tls,
                child_tid
            ))
        },
    };
    let child_pid = child.pid;
    if clone_flags.contains(CloneFlags::SETTLS) {
        crate::kprintln!(
            "[clone] parent={} child={} flags={:#x} parent_tid={:#x} child_tid={:#x} tls={:#x}",
            task.pid,
            child_pid,
            flags,
            parent_tid,
            child_tid
            ,
            tls
        );
    }

    if clone_flags.contains(CloneFlags::PARENT_SETTID) && parent_tid != 0 {
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                parent_tid as *mut u8,
                &child_pid as *const u32 as *const u8,
                core::mem::size_of::<u32>(),
            )
        };
        if rc != 0 {
            return Err(kerr!(
                proc,
                error,
                Errno::Efault,
                "sys_clone: parent_tid copyout failed ptr={:#x} child_pid={}",
                parent_tid,
                child_pid
            ));
        }
    }

    let cpu = current().cpu_id;
    spawn_user_task(child, cpu);

    // CLONE_VFORK: parent blocks until child exits or execs
    if let Some(vfork) = vfork_done {
        while !vfork.is_done() {
            core::hint::spin_loop();
        }
    }

    Ok(child_pid as usize)
}

pub fn sys_exit(task: &Arc<Task>, status: i32) {
    let wstatus = WaitStatus::exited(status);
    do_exit(task, wstatus);
}

pub async fn sys_execve_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    argv_ptr: usize,
    envp_ptr: usize,
) -> Result<(usize, usize, usize, usize), Errno> {
    // Read pathname from user memory
    let raw_path = match copyinstr(task, pathname_ptr, 256).await {
        None => {
            return Err(kerr!(
                proc,
                debug,
                Errno::Efault,
                "sys_execve: copyinstr failed ptr={:#x}",
                pathname_ptr
            ))
        },
        Some(s) => s,
    };
    let path = crate::fs::absolutize_path(task, dirfd, &raw_path)?;
    // Read argv array from user memory (before exec destroys address space)
    let argv = copyin_argv(task, argv_ptr, 64, 4096).await;
    // Read envp array
    let envp = copyin_argv(task, envp_ptr, 64, 4096).await;

    let (entry, sp) = do_execve(task, &path, &argv, &envp).await?;
    Ok((entry, sp, argv.len(), envp.len()))
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
        fault_in_user_buffer(task, wstatus_ptr, 4, PageFaultAccessType::WRITE).await;
    }

    // Check if there are any children at all
    {
        let children = task.children.lock();
        if children.is_empty() {
            return Err(kerr!(
                proc,
                debug,
                Errno::Echild,
                "sys_wait4: no children pid={}",
                task.pid
            ));
        }
    }

    if options & WNOHANG != 0 {
        // Non-blocking: scan for a zombie child, return immediately
        let children = task.children.lock();
        for child in children.iter() {
            if child.state() == TaskState::Zombie {
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
                let mut sig_chld = SigSet::empty();
                sig_chld.add(Signal::new_unchecked(SIGCHLD));
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
                        crate::hal::copy_user_chunk(
                            wstatus_ptr as *mut u8,
                            &status as *const i32 as *const u8,
                            4,
                        )
                    };
                    if rc != 0 {
                        return Err(kerr!(
                            proc,
                            debug,
                            Errno::Efault,
                            "sys_wait4: copy wstatus failed ptr={:#x} (WNOHANG)",
                            wstatus_ptr
                        ));
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
    let result = WaitChildFuture::new(Arc::clone(task), pid).await;

    match result {
        Some((child_pid, status)) => {
            // Consume pending SIGCHLD
            let mut sig_chld = SigSet::empty();
            sig_chld.add(Signal::new_unchecked(SIGCHLD));
            task.signals
                .pending
                .fetch_difference(sig_chld, core::sync::atomic::Ordering::Release);
            // Write status to user memory if pointer is non-null
            if wstatus_ptr != 0 {
                let rc = unsafe {
                    crate::hal::copy_user_chunk(
                        wstatus_ptr as *mut u8,
                        &status as *const i32 as *const u8,
                        4,
                    )
                };
                if rc != 0 {
                    return Err(kerr!(
                        proc,
                        debug,
                        Errno::Efault,
                        "sys_wait4: copy wstatus failed ptr={:#x}",
                        wstatus_ptr
                    ));
                }
            }
            Ok(child_pid)
        },
        None => {
            if task.signals.has_actionable_pending() {
                Err(kerr!(
                    proc,
                    trace,
                    Errno::Eintr,
                    "sys_wait4: interrupted by signal pid={}",
                    task.pid
                ))
            } else {
                Err(kerr!(
                    proc,
                    debug,
                    Errno::Echild,
                    "sys_wait4: no zombie child pid={}",
                    task.pid
                ))
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Process Group
// ---------------------------------------------------------------------------

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
        Err(kerr!(
            proc,
            debug,
            Errno::Esrch,
            "sys_setpgid: pid not found pid={}",
            target_pid
        ))
    }
}

pub fn sys_getpgid(task: &Arc<Task>, pid: u32) -> Result<usize, Errno> {
    if pid == 0 {
        Ok(task.pgid.load(Ordering::Relaxed) as usize)
    } else if let Some(t) = find_task_by_pid(pid) {
        Ok(t.pgid.load(Ordering::Relaxed) as usize)
    } else {
        Err(kerr!(
            proc,
            debug,
            Errno::Esrch,
            "sys_getpgid: pid not found pid={}",
            pid
        ))
    }
}

// ITIMER_REAL = 0: delivers SIGALRM when value expires
const ITIMER_REAL: usize = 0;

/// sys_getitimer: return zeros (we don't persist interval timer state)
pub fn sys_getitimer(_task: &Arc<Task>, _which: usize, value_ptr: usize) -> KernelResult<usize> {
    if value_ptr != 0 {
        // Write zeroed itimerval (4 * u64 = 32 bytes)
        let zeros = [0u64; 4];
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                value_ptr as *mut u8,
                zeros.as_ptr() as *const u8,
                32,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }
    Ok(0)
}

/// sys_setitimer: set an interval timer.
///
/// `struct itimerval` layout (RISC-V 64-bit):
///   [0..8]   it_interval.tv_sec
///   [8..16]  it_interval.tv_usec
///   [16..24] it_value.tv_sec
///   [24..32] it_value.tv_usec
///
/// Only ITIMER_REAL is supported; it spawns a kernel task that delivers
/// SIGALRM after `it_value` time, then repeats at `it_interval` intervals.
pub fn sys_setitimer(
    task: &Arc<Task>,
    which: usize,
    new_ptr: usize,
    old_ptr: usize,
) -> KernelResult<usize> {
    if which != ITIMER_REAL {
        return Err(Errno::Einval);
    }

    // Write zeroed old value if requested
    if old_ptr != 0 {
        let zeros = [0u64; 4];
        let rc = unsafe {
            crate::hal::copy_user_chunk(old_ptr as *mut u8, zeros.as_ptr() as *const u8, 32)
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    if new_ptr == 0 {
        return Ok(0);
    }

    // Read new itimerval
    let mut val = [0u64; 4];
    let rc = unsafe {
        crate::hal::copy_user_chunk(val.as_mut_ptr() as *mut u8, new_ptr as *const u8, 32)
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }

    let value_sec = val[2];
    let value_usec = val[3];
    let interval_sec = val[0];
    let interval_usec = val[1];

    let initial_ms = value_sec * 1000 + value_usec / 1000;
    let interval_ms = interval_sec * 1000 + interval_usec / 1000;

    if initial_ms == 0 {
        // Disarm timer - just return success
        return Ok(0);
    }

    let cpu = current().cpu_id;
    let task_clone = Arc::clone(task);
    spawn_kernel_task(
        async move {
            sleep(initial_ms).await;
            task_clone.signals.post_signal(crate::proc::signal::SIGALRM);
            // Wake the task if it is parked in a blocking syscall (net I/O, futex, etc.)
            if let Some(w) = task_clone.top_level_waker.lock().take() {
                w.wake();
            }
            if interval_ms > 0 {
                loop {
                    sleep(interval_ms).await;
                    task_clone.signals.post_signal(crate::proc::signal::SIGALRM);
                    if let Some(w) = task_clone.top_level_waker.lock().take() {
                        w.wake();
                    }
                }
            }
        },
        cpu,
    )
    .detach();

    Ok(0)
}
