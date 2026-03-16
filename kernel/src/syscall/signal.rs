//! Signal related system calls.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

// use crate::klog;
use crate::hal_common::Errno;
use crate::proc::{SigAction, SigFrame, Signal, Task, MAX_SIG, SIGFRAME_SIZE, SIGKILL, SIGSTOP};

pub fn sys_sigreturn(task: &Arc<Task>) -> Result<(), Errno> {
    klog!(signal, debug, "sigreturn pid={}", task.pid);
    let sp = task.trap_frame.lock().sp(); // current SP points to sigframe

    // Copyin the sigframe from user memory
    let mut frame = core::mem::MaybeUninit::<SigFrame>::uninit();
    let ok = unsafe {
        crate::hal::copy_user_chunk(
            frame.as_mut_ptr() as *mut u8,
            sp as *const u8,
            SIGFRAME_SIZE,
        )
    };
    if ok != 0 {
        return Err(Errno::Efault);
    }
    let frame = unsafe { frame.assume_init() };

    crate::hal::signal_abi::validate_sigreturn_frame(&frame.saved_tf)?;

    {
        let mut tf = task.trap_frame.lock();
        crate::hal::restore_after_sigreturn(&mut tf, &frame.saved_tf)?;
    }

    // Restore signal mask
    task.signals.blocked.store(
        crate::proc::SigSet::from_u64(frame.saved_mask),
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
            crate::hal::copy_user_chunk(
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
            crate::hal::copy_user_chunk(
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
            crate::hal::copy_user_chunk(
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
            crate::hal::copy_user_chunk(
                &mut new_set as *mut u64 as *mut u8,
                set_ptr as *const u8,
                8,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }

        let unblockable = crate::proc::SigSet::empty()
            .add(Signal::new_unchecked(SIGKILL))
            .add(Signal::new_unchecked(SIGSTOP))
            .as_u64();

        new_set &= !unblockable;
        let set = crate::proc::SigSet::from_u64(new_set);

        match how {
            SIG_BLOCK => {
                sig_state.blocked.fetch_union(set, Ordering::Release);
            },
            SIG_UNBLOCK => {
                sig_state.blocked.fetch_difference(set, Ordering::Release);
            },
            SIG_SETMASK => {
                sig_state.blocked.store(set, Ordering::Release);
            },
            _ => return Err(Errno::Einval),
        }
    }

    Ok(0)
}

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
        let target = crate::proc::find_task_by_pid(pid as u32);
        match target {
            Some(t) => {
                if sig > 0 {
                    t.signals.post_signal(sig);
                    if let Some(w) = t.top_level_waker.lock().take() {
                        w.wake();
                    }
                }
                Ok(0)
            },
            None => Err(Errno::Esrch),
        }
    } else if pid == 0 {
        let pgid = sender.pgid.load(Ordering::Relaxed);
        crate::proc::kill_pgrp(pgid, sig);
        Ok(0)
    } else if pid == -1 {
        crate::proc::for_each_task(|t| {
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
        crate::proc::kill_pgrp(pgid, sig);
        Ok(0)
    }
}
