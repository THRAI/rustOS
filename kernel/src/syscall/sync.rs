//! Synchronization and time system calls.
//!
//! Implements futex, nanosleep, clock_gettime and related operations.

use alloc::sync::Arc;

use crate::{
    hal_common::{Errno, VirtAddr},
    ipc::{futex_wait, futex_wake},
    mm::pmap_extract,
    proc::Task,
};

/// Timer frequency: QEMU virt = 10 MHz.
const TIMER_FREQ: u64 = 10_000_000;

/// Read rdtime CSR (RISC-V cycle counter used as time source).
fn read_rdtime() -> u64 {
    let val: u64;
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) val);
    }
    val
}

/// sys_clock_gettime: read hardware time.
pub fn sys_clock_gettime(task: &Arc<Task>, _clockid: u32, tp: usize) -> Result<(), Errno> {
    let _ = task; // used for user memory access context
    if tp == 0 {
        return Err(Errno::Efault);
    }

    let ticks = read_rdtime();
    let secs = ticks / TIMER_FREQ;
    let nsecs = (ticks % TIMER_FREQ) * 1_000_000_000 / TIMER_FREQ;

    // struct timespec { time_t tv_sec; long tv_nsec; } — 16 bytes on rv64
    let ts: [u64; 2] = [secs, nsecs];
    let rc = unsafe { crate::hal::copy_user_chunk(tp as *mut u8, ts.as_ptr() as *const u8, 16) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(())
}

/// sys_gettimeofday: return wall-clock time as timeval {sec, usec}.
///
/// The second argument (timezone) is ignored for compatibility.
pub fn sys_gettimeofday(task: &Arc<Task>, tv: usize, _tz: usize) -> Result<(), Errno> {
    let _ = task;
    if tv == 0 {
        return Err(Errno::Efault);
    }

    let ticks = read_rdtime();
    let secs = ticks / TIMER_FREQ;
    let usecs = (ticks % TIMER_FREQ) * 1_000_000 / TIMER_FREQ;

    // struct timeval { long tv_sec; long tv_usec; } on rv64 => 16 bytes
    let tv_out: [u64; 2] = [secs, usecs];
    let rc =
        unsafe { crate::hal::copy_user_chunk(tv as *mut u8, tv_out.as_ptr() as *const u8, 16) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(())
}

/// sys_nanosleep_async: sleep for specified time.
pub async fn sys_nanosleep_async(
    task: &Arc<Task>,
    req_ptr: usize,
    rem_ptr: usize,
) -> Result<(), Errno> {
    if req_ptr == 0 {
        return Err(Errno::Efault);
    }

    // Read struct timespec from user memory
    let mut ts = [0u64; 2];
    let rc = unsafe {
        crate::hal::copy_user_chunk(ts.as_mut_ptr() as *mut u8, req_ptr as *const u8, 16)
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    let secs = ts[0];
    let nsecs = ts[1];
    let total_ms = secs * 1000 + nsecs / 1_000_000;

    if total_ms == 0 {
        crate::executor::yield_now().await;
        return Ok(());
    }

    // Interruptible sleep: poll in 10ms increments to check signals
    let start = crate::hal::read_time_ms();
    let deadline = start + total_ms;

    loop {
        if task.signals.has_actionable_pending() {
            // Write remaining time to rem pointer
            if rem_ptr != 0 {
                let now = crate::hal::read_time_ms();
                let remaining_ms = deadline.saturating_sub(now);
                let rem_secs = remaining_ms / 1000;
                let rem_nsecs = (remaining_ms % 1000) * 1_000_000;
                let rem_ts = [rem_secs, rem_nsecs];
                let _ = unsafe {
                    crate::hal::copy_user_chunk(
                        rem_ptr as *mut u8,
                        rem_ts.as_ptr() as *const u8,
                        16,
                    )
                };
            }
            return Err(Errno::Eintr);
        }

        let now = crate::hal::read_time_ms();
        if now >= deadline {
            break;
        }

        // Sleep in small increments (max 10ms) to stay responsive to signals
        let remaining = deadline - now;
        let chunk = core::cmp::min(remaining, 10);
        crate::executor::sleep(chunk).await;
    }

    // On normal completion, write zero remaining time
    if rem_ptr != 0 {
        let zero_ts = [0u64; 2];
        let _ = unsafe {
            crate::hal::copy_user_chunk(rem_ptr as *mut u8, zero_ts.as_ptr() as *const u8, 16)
        };
    }
    Ok(())
}

/// sys_futex_async: fast userspace mutex.
pub async fn sys_futex_async(
    task: &Arc<Task>,
    uaddr: usize,
    op: u32,
    val: u32,
    timeout_ptr: usize,
) -> Result<usize, Errno> {
    const FUTEX_WAIT: u32 = 0;
    const FUTEX_WAKE: u32 = 1;
    // Mask out FUTEX_PRIVATE_FLAG (128) and FUTEX_CLOCK_REALTIME (256)
    let cmd = op & 0x7f;

    match cmd {
        FUTEX_WAIT => {
            // Read current value at uaddr via safe user-copy path.
            let mut current: u32 = 0;
            let rc = unsafe {
                crate::hal::copy_user_chunk(
                    &mut current as *mut u32 as *mut u8,
                    uaddr as *const u8,
                    core::mem::size_of::<u32>(),
                )
            };
            if rc != 0 {
                kprintln!("[futex] WAIT EFAULT uaddr={:#x}", uaddr);
                return Err(Errno::Efault);
            }
            if current != val {
                // Value changed — don't sleep
                kprintln!(
                    "[futex] WAIT EAGAIN uaddr={:#x} cur={} want={}",
                    uaddr,
                    current,
                    val
                );
                return Err(Errno::Eagain);
            }
            // Resolve physical address for futex key
            let pa = {
                let vm_map = task.vm_map.read();
                let pmap = vm_map.pmap_lock();
                pmap_extract(&pmap, VirtAddr::new(uaddr & !0xFFF)).ok_or(Errno::Efault)?
            };
            let pa_key = pa + (uaddr & 0xFFF);

            // Parse optional timeout: struct timespec { u64 tv_sec; u64 tv_nsec; }
            // deadline_ms=0 means no timeout.
            let deadline_ms: u64 = if timeout_ptr != 0 {
                let mut ts = [0u64; 2];
                let rc = unsafe {
                    crate::hal::copy_user_chunk(
                        ts.as_mut_ptr() as *mut u8,
                        timeout_ptr as *const u8,
                        16,
                    )
                };
                if rc != 0 {
                    return Err(Errno::Efault);
                }
                let timeout_ms = ts[0] * 1000 + ts[1] / 1_000_000;
                let dl = crate::hal::read_time_ms() + timeout_ms.max(1);
                // Spawn a timer that wakes the task when timeout expires so FutexWaitFuture
                // gets re-polled and can detect the deadline.
                let task2 = Arc::clone(task);
                let cpu = crate::executor::current().cpu_id;
                crate::executor::spawn_kernel_task(
                    async move {
                        crate::executor::sleep(timeout_ms.max(1)).await;
                        if let Some(w) = task2.top_level_waker.lock().take() {
                            w.wake();
                        }
                    },
                    cpu,
                ).detach();
                dl
            } else {
                0
            };

            // Park on the futex (FutexWaitFuture handles timeout and EINTR internally)
            let ret = futex_wait(pa_key, task, deadline_ms).await;
            match ret {
                Ok(()) => {
                    kprintln!("[futex] WAIT woke pid={} key={:#x}", task.pid, pa_key);
                    Ok(0)
                },
                Err(Errno::Etimedout) => {
                    kprintln!("[futex] WAIT timeout pid={} key={:#x}", task.pid, pa_key);
                    Err(Errno::Etimedout)
                },
                Err(e) => {
                    kprintln!("[futex] WAIT interrupted pid={} key={:#x} err={:?}", task.pid, pa_key, e);
                    Err(e)
                },
            }?;
            Ok(0)
        },
        FUTEX_WAKE => {
            // Resolve physical address for futex key
            let pa = {
                let vm_map = task.vm_map.read();
                let pmap = vm_map.pmap_lock();
                pmap_extract(&pmap, VirtAddr::new(uaddr & !0xFFF)).ok_or(Errno::Efault)?
            };
            let pa_key = pa + (uaddr & 0xFFF);
            let woken = futex_wake(pa_key, val as usize);
            kprintln!("[futex] WAKE pid={} key={:#x} n={}", task.pid, pa_key, woken);
            Ok(woken)
        },
        _ => Err(Errno::Enosys),
    }
}
