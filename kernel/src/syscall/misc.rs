//! Miscellaneous system calls.
//!
//! Implements uname and other utility syscalls.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{hal_common::{Errno, PAGE_SIZE}, proc::Task};

const ROBUST_LIST_HEAD_SIZE_64: usize = 24;
const RLIMIT_COUNT: u32 = 16;
const RLIMIT_STACK: u32 = 3;
const RLIM_INFINITY: u64 = u64::MAX;

static GETRANDOM_STATE: AtomicU64 = AtomicU64::new(0x6a09_e667_f3bc_c909);

#[repr(C)]
struct LinuxSysinfo {
    uptime: i64,
    loads: [u64; 3],
    totalram: u64,
    freeram: u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap: u64,
    procs: u16,
    pad: u16,
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
}

const FALLBACK_DMESG: &[u8] = b"la64 kernel log buffer not implemented yet\n";

#[repr(C)]
struct LinuxRlimit {
    rlim_cur: u64,
    rlim_max: u64,
}

fn default_rlimit(resource: u32) -> LinuxRlimit {
    match resource {
        RLIMIT_STACK => LinuxRlimit {
            rlim_cur: 8 * 1024 * 1024,
            rlim_max: RLIM_INFINITY,
        },
        _ => LinuxRlimit {
            rlim_cur: RLIM_INFINITY,
            rlim_max: RLIM_INFINITY,
        },
    }
}

#[inline]
fn next_random_u64() -> u64 {
    let t = crate::hal::monotonic_ms() as u64;
    let prev = GETRANDOM_STATE.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::AcqRel);
    let mut x = prev ^ t.rotate_left(17) ^ 0xa076_1d64_78bd_642f;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// sys_uname: get system information.
pub fn sys_uname(task: &Arc<Task>, buf: usize) -> Result<(), Errno> {
    let _ = task;
    if buf == 0 {
        return Err(Errno::Efault);
    }
    // struct utsname: 6 fields × 65 bytes = 390 bytes
    const FIELD_LEN: usize = 65;
    let mut utsname = [0u8; FIELD_LEN * 6];

    fn write_field(buf: &mut [u8], offset: usize, s: &[u8]) {
        let end = s.len().min(64);
        buf[offset..offset + end].copy_from_slice(&s[..end]);
        // already zero-filled, NUL terminator is implicit
    }

    write_field(&mut utsname, 0, b"FreeBSD"); // sysname
    write_field(&mut utsname, FIELD_LEN, b"chronix"); // nodename
    write_field(&mut utsname, FIELD_LEN * 2, b"0.1.0"); // release
    write_field(&mut utsname, FIELD_LEN * 3, b"chronix 0.1.0"); // version
    #[cfg(target_arch = "loongarch64")]
    write_field(&mut utsname, FIELD_LEN * 4, b"loongarch64"); // machine
    #[cfg(not(target_arch = "loongarch64"))]
    write_field(&mut utsname, FIELD_LEN * 4, b"riscv64"); // machine
    write_field(&mut utsname, FIELD_LEN * 5, b"(none)"); // domainname

    let rc =
        unsafe { crate::hal::copy_user_chunk(buf as *mut u8, utsname.as_ptr(), utsname.len()) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(())
}

/// sys_sysinfo: return coarse system-wide memory and uptime information.
pub fn sys_sysinfo(task: &Arc<Task>, info_ptr: usize) -> Result<(), Errno> {
    let _ = task;
    if info_ptr == 0 {
        return Err(Errno::Efault);
    }

    let totalram = (crate::mm::frame_total_pages() * PAGE_SIZE) as u64;
    let freeram = (crate::mm::frame_available_pages() * PAGE_SIZE) as u64;
    let uptime = (crate::hal::monotonic_ms() / 1000) as i64;
    let info = LinuxSysinfo {
        uptime,
        loads: [0; 3],
        totalram,
        freeram,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 1,
        pad: 0,
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
    };

    let rc = unsafe {
        crate::hal::copy_user_chunk(
            info_ptr as *mut u8,
            &info as *const LinuxSysinfo as *const u8,
            core::mem::size_of::<LinuxSysinfo>(),
        )
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(())
}

/// sys_syslog: minimal klogctl compatibility for busybox `dmesg`.
pub fn sys_syslog(task: &Arc<Task>, action: i32, buf: usize, len: i32) -> Result<usize, Errno> {
    let _ = task;
    if len < 0 {
        return Err(Errno::Einval);
    }

    match action {
        0 | 1 | 5 | 6 | 7 | 8 => Ok(0),
        9 => Ok(0), // unread bytes
        10 => Ok(FALLBACK_DMESG.len()), // total buffer size
        2 | 3 | 4 => {
            if buf == 0 && len != 0 {
                return Err(Errno::Efault);
            }
            let copy_len = core::cmp::min(len as usize, FALLBACK_DMESG.len());
            if copy_len == 0 {
                return Ok(0);
            }
            let rc =
                unsafe { crate::hal::copy_user_chunk(buf as *mut u8, FALLBACK_DMESG.as_ptr(), copy_len) };
            if rc != 0 {
                return Err(Errno::Efault);
            }
            Ok(copy_len)
        },
        _ => Err(Errno::Einval),
    }
}

/// sys_set_robust_list: glibc startup probe for PI futex robust lists.
///
/// We do not implement robust futex teardown yet, but accepting the call is
/// enough for single-threaded startup paths used by the current autotests.
pub fn sys_set_robust_list(_task: &Arc<Task>, head: usize, len: usize) -> Result<usize, Errno> {
    let _ = head;
    if len != ROBUST_LIST_HEAD_SIZE_64 {
        return Err(Errno::Einval);
    }
    Ok(0)
}

/// sys_prlimit64: minimal getrlimit/setrlimit compatibility for glibc startup.
pub fn sys_prlimit64(
    task: &Arc<Task>,
    pid: u32,
    resource: u32,
    new_limit_ptr: usize,
    old_limit_ptr: usize,
) -> Result<usize, Errno> {
    if resource >= RLIMIT_COUNT {
        return Err(Errno::Einval);
    }
    if pid != 0 && pid != task.pid {
        return Err(Errno::Esrch);
    }

    if new_limit_ptr != 0 {
        let mut tmp = LinuxRlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                &mut tmp as *mut LinuxRlimit as *mut u8,
                new_limit_ptr as *const u8,
                core::mem::size_of::<LinuxRlimit>(),
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    if old_limit_ptr != 0 {
        let limit = default_rlimit(resource);
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                old_limit_ptr as *mut u8,
                &limit as *const LinuxRlimit as *const u8,
                core::mem::size_of::<LinuxRlimit>(),
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    Ok(0)
}

/// sys_getrandom: fill a user buffer with pseudorandom bytes.
pub fn sys_getrandom(
    _task: &Arc<Task>,
    buf: usize,
    len: usize,
    flags: u32,
) -> Result<usize, Errno> {
    if buf == 0 && len != 0 {
        return Err(Errno::Efault);
    }
    if flags & !0x7 != 0 {
        return Err(Errno::Einval);
    }
    if len == 0 {
        return Ok(0);
    }

    let mut remaining = len;
    let mut offset = 0usize;
    while remaining != 0 {
        let rand = next_random_u64().to_ne_bytes();
        let chunk = remaining.min(rand.len());
        let rc =
            unsafe { crate::hal::copy_user_chunk((buf + offset) as *mut u8, rand.as_ptr(), chunk) };
        if rc != 0 {
            return Err(Errno::Efault);
        }
        remaining -= chunk;
        offset += chunk;
    }

    Ok(len)
}
