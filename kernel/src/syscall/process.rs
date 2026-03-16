//! Process management system calls.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::{
    executor::{current, spawn_user_task},
    hal_common::{Errno, KernelResult},
    mm::PageFaultAccessType,
    proc::{
        copyin_argv, copyinstr, do_clone, do_execve, do_exit, fault_in_user_buffer,
        find_task_by_pid, CloneFlags, SigSet, Signal, Task, TaskState, WaitChildFuture, WaitStatus,
        SIGCHLD,
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
    let clone_flags = CloneFlags::from_bits_truncate(flags as u64);

    // CLONE_THREAD still not supported
    if clone_flags.contains(CloneFlags::THREAD) {
        return Err(kerr!(
            proc,
            debug,
            Errno::Enosys,
            "sys_clone: CLONE_THREAD not supported"
        ));
    }

    let (child, vfork_done) = do_clone(task, clone_flags, child_stack, tls)?;
    let child_pid = child.pid;
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
