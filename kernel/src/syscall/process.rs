//! Process management system calls.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::executor::user_task::spawn_user_task;
use crate::klog;
use crate::proc::signal::Signal;
use crate::proc::task::Task;
use crate::proc::user_copy::{copyin_argv, copyinstr, do_exit, fault_in_user_buffer};
use hal_common::Errno;

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

// TODO: handle flags
pub fn sys_clone(task: &Arc<Task>) -> usize {
    // Basic fork (flags ignored for now)
    let child = crate::proc::fork::fork(task);
    let child_pid = child.pid;
    // Spawn child on same CPU
    let cpu = crate::executor::per_cpu::current().cpu_id;
    spawn_user_task(child, cpu);
    child_pid as usize
}

pub fn sys_exit(task: &Arc<Task>, status: i32) {
    let wstatus = crate::proc::exit_wait::WaitStatus::exited(status);
    do_exit(task, wstatus);
}

pub async fn sys_execve_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    argv_ptr: usize,
    envp_ptr: usize,
) -> Result<(usize, usize), Errno> {
    // Read pathname from user memory
    let raw_path = match copyinstr(task, pathname_ptr, 256).await {
        None => return Err(Errno::EFAULT),
        Some(s) => s,
    };
    let path = super::fs::absolutize_path(task, dirfd, &raw_path)?;
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
            return Err(Errno::ECHILD);
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
                let status = child.exit_status.load(core::sync::atomic::Ordering::Acquire);
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
                        return Err(Errno::EFAULT);
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
                    return Err(Errno::EFAULT);
                }
            }
            Ok(child_pid)
        }
        None => {
            if task.signals.has_actionable_pending() {
                Err(Errno::EINTR)
            } else {
                Err(Errno::ECHILD)
            }
        }
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
        Err(Errno::ESRCH)
    }
}

pub fn sys_getpgid(task: &Arc<Task>, pid: u32) -> Result<usize, Errno> {
    if pid == 0 {
        Ok(task.pgid.load(Ordering::Relaxed) as usize)
    } else if let Some(t) = crate::proc::signal::find_task_by_pid(pid) {
        Ok(t.pgid.load(Ordering::Relaxed) as usize)
    } else {
        Err(Errno::ESRCH)
    }
}
