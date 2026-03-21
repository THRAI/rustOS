//! Signal related system calls.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

// use crate::klog;
use crate::hal_common::Errno;
use crate::proc::{
    SigAction, SigFrame, SigSet, Signal, Task, MAX_SIG, SIGFRAME_SIZE, SIGKILL, SIGSTOP,
};

pub fn sys_sigreturn(task: &Arc<Task>) -> Result<(), Errno> {
    klog!(signal, debug, "sigreturn pid={}", task.pid);
    let sp = task.trap_frame.lock().x[2]; // current SP points to sigframe

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

    // Validate sepc: must be in user space (< 0x0000_0040_0000_0000)
    const USER_MAX_VA: usize = 0x0000_0040_0000_0000;
    if frame.uc.uc_gregs[0] as usize >= USER_MAX_VA {
        return Err(Errno::Einval);
    }

    // Restore trap frame from user-provided ucontext (Linux rt_sigreturn semantics).
    {
        let mut tf = task.trap_frame.lock();
        tf.x = [0usize; 32];
        tf.sepc = frame.uc.uc_gregs[0] as usize;
        for (i, reg) in frame.uc.uc_gregs.iter().enumerate().skip(1) {
            tf.x[i] = *reg as usize;
        }
        // Sanitize sstatus: SPP cleared (user mode), SPIE set, FS>=Initial
        tf.sstatus = (tf.sstatus & !(1 << 8)) | (1 << 5);
        if tf.sstatus & (3 << 13) == 0 {
            tf.sstatus |= 1 << 13; // FS=Initial if Off
        }
    }

    // Restore signal mask
    task.signals.blocked.store(
        crate::proc::SigSet::from_u64(frame.uc.uc_sigmask[0]),
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

const SIGSET_SIZE: usize = 8;
const TIMESPEC_SIZE: usize = 16;
const SIGINFO_SIZE: usize = 128;

fn dequeue_pending_from_set(task: &Arc<Task>, wait_set: SigSet) -> Option<u8> {
    let sig_state = &task.signals;
    loop {
        let pending = sig_state.pending.load(Ordering::Acquire);

        // SIGKILL/SIGSTOP are never waitable via sigtimedwait.
        let mut unblockable = SigSet::empty();
        unblockable
            .add(Signal::new_unchecked(SIGKILL))
            .add(Signal::new_unchecked(SIGSTOP));
        let eligible = pending.intersect(wait_set).difference(unblockable);

        if eligible.is_empty() {
            return None;
        }

        let bit = eligible.as_u64().trailing_zeros() as u8;
        let sig = Signal::new_unchecked(bit + 1);
        let old = sig_state.pending.fetch_remove(sig, Ordering::AcqRel);
        if old.contains(sig) {
            return Some(sig.as_u8());
        }
    }
}

fn write_min_siginfo(siginfo_ptr: usize, signo: u8) -> Result<(), Errno> {
    if siginfo_ptr == 0 {
        return Ok(());
    }

    // Minimal Linux-compatible siginfo_t head:
    // si_signo (i32 @0), si_errno (i32 @4), si_code (i32 @8).
    let mut buf = [0u8; SIGINFO_SIZE];
    let signo_i32 = (signo as i32).to_le_bytes();
    buf[0..4].copy_from_slice(&signo_i32);
    // si_errno = 0
    let si_code_kernel = 128i32.to_le_bytes(); // SI_KERNEL
    buf[8..12].copy_from_slice(&si_code_kernel);

    let rc = unsafe { crate::hal::copy_user_chunk(siginfo_ptr as *mut u8, buf.as_ptr(), SIGINFO_SIZE) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(())
}

pub async fn sys_rt_sigtimedwait(
    task: &Arc<Task>,
    set_ptr: usize,
    info_ptr: usize,
    timeout_ptr: usize,
    sigsetsize: usize,
) -> Result<usize, Errno> {
    if set_ptr == 0 {
        return Err(Errno::Efault);
    }
    if sigsetsize < SIGSET_SIZE {
        return Err(Errno::Einval);
    }

    let mut raw_set = 0u64;
    let rc = unsafe {
        crate::hal::copy_user_chunk(
            &mut raw_set as *mut u64 as *mut u8,
            set_ptr as *const u8,
            SIGSET_SIZE,
        )
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    let wait_set = SigSet::from_u64(raw_set);

    // timeout == NULL means wait indefinitely.
    let mut deadline_ms: Option<u64> = None;
    if timeout_ptr != 0 {
        let mut ts = [0u64; 2];
        let rc = unsafe {
            crate::hal::copy_user_chunk(ts.as_mut_ptr() as *mut u8, timeout_ptr as *const u8, TIMESPEC_SIZE)
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
        let secs = ts[0];
        let nsecs = ts[1];
        if nsecs >= 1_000_000_000 {
            return Err(Errno::Einval);
        }
        let timeout_ms = secs
            .saturating_mul(1000)
            .saturating_add((nsecs + 999_999) / 1_000_000);
        deadline_ms = Some(crate::hal::read_time_ms().saturating_add(timeout_ms));
    }

    loop {
        if let Some(sig) = dequeue_pending_from_set(task, wait_set) {
            write_min_siginfo(info_ptr, sig)?;
            return Ok(sig as usize);
        }

        if let Some(deadline) = deadline_ms {
            let now = crate::hal::read_time_ms();
            if now >= deadline {
                return Err(Errno::Eagain);
            }
        }

        // Signals outside wait_set should still interrupt this syscall.
        if task.signals.has_actionable_pending() {
            return Err(Errno::Eintr);
        }

        let sleep_ms = if let Some(deadline) = deadline_ms {
            let now = crate::hal::read_time_ms();
            core::cmp::min(deadline.saturating_sub(now), 10)
        } else {
            10
        };
        if sleep_ms == 0 {
            crate::executor::yield_now().await;
        } else {
            crate::executor::sleep(sleep_ms).await;
        }
    }
}

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

pub fn sys_tkill(_sender: &Arc<Task>, tid: isize, sig: u8) -> Result<usize, Errno> {
    if tid <= 0 || sig > MAX_SIG {
        return Err(Errno::Einval);
    }
    let target = crate::proc::find_task_by_pid(tid as u32).ok_or(Errno::Esrch)?;
    if sig > 0 {
        target.signals.post_signal(sig);
        if let Some(w) = target.top_level_waker.lock().take() {
            w.wake();
        }
    }
    Ok(0)
}

pub fn sys_tgkill(_sender: &Arc<Task>, tgid: isize, tid: isize, sig: u8) -> Result<usize, Errno> {
    if tgid <= 0 || tid <= 0 || sig > MAX_SIG {
        return Err(Errno::Einval);
    }
    let target = crate::proc::find_task_by_pid(tid as u32).ok_or(Errno::Esrch)?;
    if target.tgid != tgid as u32 {
        return Err(Errno::Esrch);
    }
    if sig > 0 {
        target.signals.post_signal(sig);
        if let Some(w) = target.top_level_waker.lock().take() {
            w.wake();
        }
    }
    Ok(0)
}
