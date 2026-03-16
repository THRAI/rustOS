//! Process management system calls.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::{
    executor::{current, spawn_user_task},
    hal_common::{Errno, KernelResult},
    mm::PageFaultAccessType,
    proc::{
        copyin_argv, copyinstr, do_execve, do_exit, fault_in_user_buffer, find_task_by_pid, fork,
        SigSet, Signal, Task, TaskState, WaitChildFuture, WaitStatus, SIGCHLD,
    },
};

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

pub fn sys_clone(
    task: &Arc<Task>,
    flags: usize,
    child_stack: usize,
    _parent_tid: usize,
    tls: usize,
    _child_tid: usize,
) -> KernelResult<usize> {
    // Minimal clone support:
    // - process-style clone/fork is supported
    // - thread-group clone is not supported yet
    const CLONE_THREAD: usize = 0x0001_0000;
    const CLONE_SETTLS: usize = 0x0008_0000;

    if flags & CLONE_THREAD != 0 {
        return Err(Errno::Enosys);
    }

    let child = fork(task);

    // If caller supplied a child stack, override sp in the child's trap frame.
    // musl __clone stores fn/arg on this stack before ecall; child reads them via sp.
    if child_stack != 0 || ((flags & CLONE_SETTLS) != 0 && tls != 0) {
        let mut child_tf = child.trap_frame.lock();
        crate::hal::syscall_abi::setup_clone_child(
            &mut child_tf,
            (child_stack != 0).then_some(child_stack),
            ((flags & CLONE_SETTLS) != 0 && tls != 0).then_some(tls),
        );
    }

    let child_pid = child.pid;
    let cpu = current().cpu_id;
    spawn_user_task(child, cpu);
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
        None => return Err(Errno::Efault),
        Some(s) => s,
    };
    let path = super::fs::absolutize_path(task, dirfd, &raw_path)?;
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
            return Err(Errno::Echild);
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
                    return Err(Errno::Efault);
                }
            }
            Ok(child_pid)
        },
        None => {
            if task.signals.has_actionable_pending() {
                Err(Errno::Eintr)
            } else {
                Err(Errno::Echild)
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
        Err(Errno::Esrch)
    }
}

pub fn sys_getpgid(task: &Arc<Task>, pid: u32) -> Result<usize, Errno> {
    if pid == 0 {
        Ok(task.pgid.load(Ordering::Relaxed) as usize)
    } else if let Some(t) = find_task_by_pid(pid) {
        Ok(t.pgid.load(Ordering::Relaxed) as usize)
    } else {
        Err(Errno::Esrch)
    }
}
