//! Unified syscall layer for rustOS kernel.
//!
//! This module provides a centralized syscall dispatcher that routes
//! system calls to their respective implementation modules.
//!
//! Architecture inspired by Chronix and Del0n1x kernels.

use alloc::sync::Arc;

use crate::{
    hal_common::{Errno, KernelResult},
    proc::Task,
};

pub mod fs;
pub mod memory;
pub mod misc;
pub mod net;
pub mod process;
pub mod signal;
pub mod sync;

/// Syscall handling result for trap dispatch.
pub enum SyscallAction {
    /// Advance `sepc` and write return value to `a0`.
    Return(usize),
    /// Keep register state unchanged and return to trap loop.
    /// Used by `sigreturn` and restarted syscalls.
    Continue,
    /// Current task has exited.
    Exit,
}

/// System call ID enumeration (Linux-compatible rv64).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SyscallId(pub usize);

impl SyscallId {
    pub const GETCWD: Self = Self(17);
    pub const DUP: Self = Self(23);
    pub const DUP3: Self = Self(24);
    pub const IOCTL: Self = Self(29);
    pub const LINKAT: Self = Self(37);
    pub const RENAMEAT: Self = Self(38);
    pub const CHDIR: Self = Self(49);
    pub const FTRUNCATE: Self = Self(46);
    pub const FACCESSAT: Self = Self(48);
    pub const OPENAT: Self = Self(56);
    pub const CLOSE: Self = Self(57);
    pub const GETDENTS64: Self = Self(61);
    pub const LSEEK: Self = Self(62);
    pub const READ: Self = Self(63);
    pub const WRITE: Self = Self(64);
    pub const READV: Self = Self(65);
    pub const WRITEV: Self = Self(66);
    pub const SENDFILE: Self = Self(71);
    pub const PSELECT6: Self = Self(72);
    pub const READLINKAT: Self = Self(78);
    pub const FSTATAT: Self = Self(79);
    pub const SYNC: Self = Self(81);
    pub const FSYNC: Self = Self(82);
    pub const FDATASYNC: Self = Self(83);
    pub const FSTAT: Self = Self(80);
    pub const UTIMENSAT: Self = Self(88);
    pub const EXIT: Self = Self(93);
    pub const EXIT_GROUP: Self = Self(94);
    pub const SET_TID_ADDRESS: Self = Self(96);
    pub const FUTEX: Self = Self(98);
    pub const NANOSLEEP: Self = Self(101);
    pub const SETITIMER: Self = Self(103);
    pub const GETITIMER: Self = Self(102);
    pub const CLOCK_GETTIME: Self = Self(113);
    pub const SCHED_YIELD: Self = Self(124);
    pub const KILL: Self = Self(129);
    pub const TKILL: Self = Self(130);
    pub const TGKILL: Self = Self(131);
    pub const SIGALTSTACK: Self = Self(132);
    pub const SIGACTION: Self = Self(134);
    pub const SIGPROCMASK: Self = Self(135);
    pub const SIGTIMEDWAIT: Self = Self(137);
    pub const SIGRETURN: Self = Self(139);
    pub const REBOOT: Self = Self(142);
    pub const TIMES: Self = Self(153);
    pub const SETPGID: Self = Self(154);
    pub const GETPGID: Self = Self(155);
    pub const UNAME: Self = Self(160);
    pub const GETPID: Self = Self(172);
    pub const GETPPID: Self = Self(173);
    pub const GETUID: Self = Self(174);
    pub const GETEUID: Self = Self(175);
    pub const GETGID: Self = Self(176);
    pub const GETEGID: Self = Self(177);
    pub const GETTID: Self = Self(178);
    pub const BRK: Self = Self(214);
    pub const MUNMAP: Self = Self(215);
    pub const CLONE: Self = Self(220);
    pub const EXECVE: Self = Self(221);
    pub const MMAP: Self = Self(222);
    pub const MPROTECT: Self = Self(226);
    pub const WAIT4: Self = Self(260);
    pub const RENAMEAT2: Self = Self(276);
    pub const MKDIRAT: Self = Self(34);
    pub const UNLINKAT: Self = Self(35);
    pub const SYMLINKAT: Self = Self(36);
    pub const UMOUNT2: Self = Self(39);
    pub const MOUNT: Self = Self(40);
    pub const PIPE2: Self = Self(59);
    pub const FCNTL: Self = Self(25);
    pub const PPOLL: Self = Self(73);
    pub const UMASK: Self = Self(166);
    pub const GETRUSAGE: Self = Self(165);
    pub const GETTIMEOFDAY: Self = Self(169);
    // Network syscalls
    pub const SOCKET: Self = Self(198);
    pub const BIND: Self = Self(200);
    pub const LISTEN: Self = Self(201);
    pub const ACCEPT: Self = Self(202);
    pub const CONNECT: Self = Self(203);
    pub const GETSOCKNAME: Self = Self(204);
    pub const GETPEERNAME: Self = Self(205);
    pub const SENDTO: Self = Self(206);
    pub const RECVFROM: Self = Self(207);
    pub const SETSOCKOPT: Self = Self(208);
    pub const GETSOCKOPT: Self = Self(209);
    pub const SHUTDOWN: Self = Self(210);
    pub const SENDMSG: Self = Self(211);
    pub const RECVMSG: Self = Self(212);
    pub const ACCEPT4: Self = Self(242);
    // Stubs needed by musl libc init
    pub const SET_ROBUST_LIST: Self = Self(99);
    pub const CLOCK_NANOSLEEP: Self = Self(115);
    pub const SCHED_GETSCHEDULER: Self = Self(120);
    pub const SCHED_GET_PRIORITY_MAX: Self = Self(125);
    pub const SCHED_GET_PRIORITY_MIN: Self = Self(126);
    pub const PRLIMIT64: Self = Self(261);
    pub const GETRANDOM: Self = Self(278);
    pub const RSEQ: Self = Self(293);
    pub const RISCV_FLUSH_ICACHE: Self = Self(258);
}

impl core::fmt::Display for SyscallId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match *self {
            Self::GETCWD => "getcwd",
            Self::DUP => "dup",
            Self::DUP3 => "dup3",
            Self::IOCTL => "ioctl",
            Self::LINKAT => "linkat",
            Self::RENAMEAT => "renameat",
            Self::CHDIR => "chdir",
            Self::FTRUNCATE => "ftruncate",
            Self::FACCESSAT => "faccessat",
            Self::OPENAT => "openat",
            Self::CLOSE => "close",
            Self::GETDENTS64 => "getdents64",
            Self::LSEEK => "lseek",
            Self::READ => "read",
            Self::READV => "readv",
            Self::WRITE => "write",
            Self::WRITEV => "writev",
            Self::SENDFILE => "sendfile",
            Self::PSELECT6 => "pselect6",
            Self::READLINKAT => "readlinkat",
            Self::FSTATAT => "fstatat",
            Self::SYNC => "sync",
            Self::FSYNC => "fsync",
            Self::FDATASYNC => "fdatasync",
            Self::FSTAT => "fstat",
            Self::UTIMENSAT => "utimensat",
            Self::EXIT => "exit",
            Self::EXIT_GROUP => "exit_group",
            Self::SET_TID_ADDRESS => "set_tid_address",
            Self::FUTEX => "futex",
            Self::NANOSLEEP => "nanosleep",
            Self::GETITIMER => "getitimer",
            Self::SETITIMER => "setitimer",
            Self::CLOCK_GETTIME => "clock_gettime",
            Self::SCHED_YIELD => "sched_yield",
            Self::KILL => "kill",
            Self::TKILL => "tkill",
            Self::TGKILL => "tgkill",
            Self::SIGALTSTACK => "sigaltstack",
            Self::SIGACTION => "sigaction",
            Self::SIGPROCMASK => "sigprocmask",
            Self::SIGTIMEDWAIT => "rt_sigtimedwait",
            Self::SIGRETURN => "sigreturn",
            Self::REBOOT => "reboot",
            Self::TIMES => "times",
            Self::SETPGID => "setpgid",
            Self::GETPGID => "getpgid",
            Self::UNAME => "uname",
            Self::GETPID => "getpid",
            Self::GETPPID => "getppid",
            Self::GETUID => "getuid",
            Self::GETEUID => "geteuid",
            Self::GETGID => "getgid",
            Self::GETEGID => "getegid",
            Self::GETTID => "gettid",
            Self::BRK => "brk",
            Self::MUNMAP => "munmap",
            Self::CLONE => "clone",
            Self::EXECVE => "execve",
            Self::MMAP => "mmap",
            Self::MPROTECT => "mprotect",
            Self::WAIT4 => "wait4",
            Self::RENAMEAT2 => "renameat2",
            Self::MKDIRAT => "mkdirat",
            Self::UNLINKAT => "unlinkat",
            Self::SYMLINKAT => "symlinkat",
            Self::UMOUNT2 => "umount2",
            Self::MOUNT => "mount",
            Self::PIPE2 => "pipe2",
            Self::FCNTL => "fcntl",
            Self::PPOLL => "ppoll",
            Self::UMASK => "umask",
            Self::GETRUSAGE => "getrusage",
            Self::GETTIMEOFDAY => "gettimeofday",
            Self::SOCKET => "socket",
            Self::BIND => "bind",
            Self::LISTEN => "listen",
            Self::ACCEPT => "accept",
            Self::CONNECT => "connect",
            Self::GETSOCKNAME => "getsockname",
            Self::GETPEERNAME => "getpeername",
            Self::SENDTO => "sendto",
            Self::RECVFROM => "recvfrom",
            Self::SETSOCKOPT => "setsockopt",
            Self::GETSOCKOPT => "getsockopt",
            Self::SHUTDOWN => "shutdown",
            Self::SENDMSG => "sendmsg",
            Self::RECVMSG => "recvmsg",
            Self::ACCEPT4 => "accept4",
            Self::SET_ROBUST_LIST => "set_robust_list",
            Self::CLOCK_NANOSLEEP => "clock_nanosleep",
            Self::SCHED_GETSCHEDULER => "sched_getscheduler",
            Self::SCHED_GET_PRIORITY_MAX => "sched_get_priority_max",
            Self::SCHED_GET_PRIORITY_MIN => "sched_get_priority_min",
            Self::PRLIMIT64 => "prlimit64",
            Self::GETRANDOM => "getrandom",
            Self::RSEQ => "rseq",
            Self::RISCV_FLUSH_ICACHE => "riscv_flush_icache",
            _ => return write!(f, "unknown({})", self.0),
        };
        write!(f, "{}", name)
    }
}

/// Convert an [`Errno`] to the Linux-convention negative return value
/// for placement into the syscall trap frame's `a0` register.
///
/// This is the **only** function in the kernel that performs this
/// conversion.  See the encoding policy in [`crate::hal_common::errno`].
#[inline]
fn syscall_error_return(e: Errno) -> usize {
    e.as_linux_ret() as usize
}

/// SA_RESTART: check if current pending signal wants syscall restart.
fn should_restart_syscall(task: &Arc<Task>) -> bool {
    let pending = task
        .signals
        .pending
        .load(core::sync::atomic::Ordering::Acquire);
    let blocked = task
        .signals
        .blocked
        .load(core::sync::atomic::Ordering::Relaxed);

    let mut unblockable = crate::proc::SigSet::empty();
    unblockable
        .add(crate::proc::Signal::new_unchecked(crate::proc::SIGKILL))
        .add(crate::proc::Signal::new_unchecked(crate::proc::SIGSTOP));
    let deliverable = pending.intersect(blocked.union(unblockable).difference(blocked));

    if !deliverable.is_empty() {
        let bit = deliverable.as_u64().trailing_zeros() as u8;
        let sig = bit + 1;
        task.signals.is_restart(sig)
    } else {
        false
    }
}

/// Main syscall dispatcher.
///
/// Routes system calls to their respective implementation modules.
/// This function is called from the trap handler in executor/user_task.rs.
pub async fn syscall(task: &Arc<Task>, syscall_id: usize, args: [usize; 6]) -> SyscallAction {
    use crate::{mm::vm::PageFaultAccessType, proc::fault_in_user_buffer};

    const AT_FDCWD: isize = -100;

    let id = SyscallId(syscall_id);
    let [a0, a1, a2, a3, a4, a5] = args;

    crate::klog!(
        syscall,
        debug,
        "pid={} {} a0={:#x} a1={:#x} a2={:#x}",
        task.pid,
        id,
        a0,
        a1,
        a2
    );

    let action = match id {
        SyscallId::GETPID => SyscallAction::Return(process::sys_getpid(task)),
        SyscallId::GETPPID => SyscallAction::Return(process::sys_getppid(task)),
        SyscallId::GETUID => SyscallAction::Return(process::sys_getuid(task)),
        SyscallId::GETEUID => SyscallAction::Return(process::sys_geteuid(task)),
        SyscallId::GETGID => SyscallAction::Return(process::sys_getgid(task)),
        SyscallId::GETEGID => SyscallAction::Return(process::sys_getegid(task)),
        SyscallId::GETTID => SyscallAction::Return(process::sys_gettid(task)),
        SyscallId::SET_TID_ADDRESS => SyscallAction::Return(process::sys_gettid(task)),
        SyscallId::DUP => {
            let ret = match task.fd_table.lock().dup(a0 as u32) {
                Ok(fd) => fd as usize,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::DUP3 => {
            let cloexec = (a2 & 0o2000000) != 0;
            let ret = match task.fd_table.lock().dup3(a0 as u32, a1 as u32, cloexec) {
                Ok(fd) => fd as usize,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::PIPE2 => {
            let ret = match fs::sys_pipe2(task, a0, a1) {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SCHED_YIELD => {
            crate::executor::yield_now().await;
            SyscallAction::Return(0)
        },
        SyscallId::BRK => SyscallAction::Return(memory::sys_brk(task, a0)),
        SyscallId::MMAP => {
            let ret = match memory::sys_mmap(task, a0, a1, a2, a3, a4 as u32, a5 as u64) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::MUNMAP => {
            let ret = match memory::sys_munmap(task, a0, a1) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::MPROTECT => {
            let ret = match memory::sys_mprotect(task, a0, a1, a2) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SIGACTION => {
            if a1 != 0 {
                fault_in_user_buffer(task, a1, 32, PageFaultAccessType::READ).await;
            }
            if a2 != 0 {
                fault_in_user_buffer(task, a2, 32, PageFaultAccessType::WRITE).await;
            }
            let ret = match signal::sys_sigaction(task, a0, a1, a2) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SIGPROCMASK => {
            if a1 != 0 {
                fault_in_user_buffer(task, a1, 8, PageFaultAccessType::READ).await;
            }
            if a2 != 0 {
                fault_in_user_buffer(task, a2, 8, PageFaultAccessType::WRITE).await;
            }
            let ret = match signal::sys_sigprocmask(task, a0, a1, a2) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SIGTIMEDWAIT => {
            // a0=set, a1=info, a2=timeout, a3=sigsetsize
            if a0 != 0 {
                fault_in_user_buffer(task, a0, 8, PageFaultAccessType::READ).await;
            }
            if a1 != 0 {
                fault_in_user_buffer(task, a1, 128, PageFaultAccessType::WRITE).await;
            }
            if a2 != 0 {
                fault_in_user_buffer(task, a2, 16, PageFaultAccessType::READ).await;
            }
            let ret = match signal::sys_rt_sigtimedwait(task, a0, a1, a2, a3).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SIGRETURN => {
            let _ = signal::sys_sigreturn(task);
            SyscallAction::Continue
        },
        SyscallId::KILL => {
            let ret = match signal::sys_kill(task, a0 as isize, a1 as u8) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::TKILL => {
            let ret = match signal::sys_tkill(task, a0 as isize, a1 as u8) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::TGKILL => {
            let ret = match signal::sys_tgkill(task, a0 as isize, a1 as isize, a2 as u8) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SETPGID => {
            let ret = match process::sys_setpgid(task, a0 as u32, a1 as u32) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETPGID => {
            let ret = match process::sys_getpgid(task, a0 as u32) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SIGALTSTACK => SyscallAction::Return(0),
        SyscallId::CLOCK_GETTIME => {
            if a1 != 0 {
                fault_in_user_buffer(task, a1, 16, PageFaultAccessType::WRITE).await;
            }
            let ret = match sync::sys_clock_gettime(task, a0 as u32, a1) {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETTIMEOFDAY => {
            if a0 != 0 {
                fault_in_user_buffer(task, a0, 16, PageFaultAccessType::WRITE).await;
            }
            let ret = match sync::sys_gettimeofday(task, a0, a1) {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::NANOSLEEP => {
            let ret = match sync::sys_nanosleep_async(task, a0, a1).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETITIMER => {
            let ret = match process::sys_getitimer(task, a0, a1) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SETITIMER => {
            let ret = match process::sys_setitimer(task, a0, a1, a2) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::TIMES => SyscallAction::Return(0),
        SyscallId::UNAME => {
            let ret = match misc::sys_uname(task, a0) {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::IOCTL => {
            let ret = match fs::sys_ioctl_async(task, a0 as u32, a1, a2).await {
                Ok(v) => v as usize,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::LINKAT => {
            let ret = match fs::sys_linkat_async(task, a0 as isize, a1, a2 as isize, a3, a4 as i32)
                .await
            {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::RENAMEAT => {
            let ret = match fs::sys_renameat2_async(task, a0 as isize, a1, a2 as isize, a3, 0).await
            {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETCWD => {
            let ret = match fs::sys_getcwd(task, a0, a1) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::FTRUNCATE => {
            let ret = match fs::sys_ftruncate_async(task, a0 as u32, a1 as u64).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::FACCESSAT => {
            let ret =
                match fs::sys_faccessat_async(task, a0 as isize, a1, a2 as i32, a3 as i32).await {
                    Ok(()) => 0,
                    Err(e) => syscall_error_return(e),
                };
            SyscallAction::Return(ret)
        },
        SyscallId::FCNTL => {
            let ret = match fs::sys_fcntl(task, a0 as u32, a1 as u32, a2) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::WRITEV => match fs::sys_writev_async(task, a0 as u32, a1, a2).await {
            Ok(n) => SyscallAction::Return(n),
            Err(Errno::Eintr) if should_restart_syscall(task) => SyscallAction::Continue,
            Err(e) => SyscallAction::Return(syscall_error_return(e)),
        },
        SyscallId::PPOLL => match fs::sys_ppoll_async(task, a0, a1, a2).await {
            Ok(n) => SyscallAction::Return(n),
            Err(Errno::Eintr) if should_restart_syscall(task) => SyscallAction::Continue,
            Err(e) => SyscallAction::Return(syscall_error_return(e)),
        },
        SyscallId::WRITE => match fs::sys_write_async(task, a0 as u32, a1, a2).await {
            Ok(n) => SyscallAction::Return(n),
            Err(Errno::Eintr) if should_restart_syscall(task) => SyscallAction::Continue,
            Err(e) => SyscallAction::Return(syscall_error_return(e)),
        },
        SyscallId::READ => match fs::sys_read_async(task, a0 as u32, a1, a2).await {
            Ok(n) => SyscallAction::Return(n),
            Err(Errno::Eintr) if should_restart_syscall(task) => SyscallAction::Continue,
            Err(e) => SyscallAction::Return(syscall_error_return(e)),
        },
        SyscallId::READV => match fs::sys_readv_async(task, a0 as u32, a1, a2).await {
            Ok(n) => SyscallAction::Return(n),
            Err(Errno::Eintr) if should_restart_syscall(task) => SyscallAction::Continue,
            Err(e) => SyscallAction::Return(syscall_error_return(e)),
        },
        SyscallId::SENDFILE => SyscallAction::Return(syscall_error_return(Errno::Einval)),
        SyscallId::PSELECT6 => {
            let ret = match fs::sys_pselect6_async(task, a0, a1, a2, a3, a4).await {
                Ok(n) => n,
                Err(Errno::Eintr) if should_restart_syscall(task) => {
                    return SyscallAction::Continue;
                },
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::READLINKAT => {
            let ret = match fs::sys_readlinkat_async(task, a0 as isize, a1, a2, a3).await {
                Ok(n) => n,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SYNC => {
            let ret = match fs::sys_sync_async().await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::FSYNC => {
            let ret = match fs::sys_fsync_async(task, a0 as u32).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::FDATASYNC => {
            let ret = match fs::sys_fdatasync_async(task, a0 as u32).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::OPENAT => {
            let ret = match fs::sys_openat_async(task, a0 as isize, a1, a2).await {
                Ok(fd) => fd as usize,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::CHDIR => {
            let ret = match fs::sys_chdir_async(task, a0).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::CLOSE => {
            let ret = match fs::sys_close(task, a0 as u32) {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::FSTAT => {
            let ret = match fs::sys_fstat(task, a0 as u32, a1) {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::UTIMENSAT => {
            let ret = match fs::sys_utimensat_async(task, a0 as isize, a1, a2, a3).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::LSEEK => {
            let ret = match fs::sys_lseek(task, a0 as u32, a1 as i64, a2 as u32) {
                Ok(off) => off as usize,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::EXIT | SyscallId::EXIT_GROUP => {
            process::sys_exit(task, a0 as i32);
            SyscallAction::Exit
        },
        SyscallId::REBOOT => {
            crate::klog!(
                syscall,
                info,
                "reboot syscall: shutting down (cmd={:#x})",
                a2
            );
            crate::hal::shutdown();
        },
        SyscallId::CLONE => {
            let ret = match process::sys_clone(task, a0, a1, a2, a3, a4) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::EXECVE => {
            match process::sys_execve_async(task, AT_FDCWD, a0, a1, a2).await {
                Ok((entry, sp, argc, envc)) => {
                    let ptr_sz = core::mem::size_of::<usize>();
                    let argv_ptr = sp + ptr_sz;
                    let envp_ptr = argv_ptr + (argc + 1) * ptr_sz;
                    let _ = envc;
                    let mut tf = task.trap_frame.lock();
                    tf.sepc = entry;
                    tf.x[2] = sp;
                    for i in 1..32 {
                        if i != 2 {
                            tf.x[i] = 0;
                        }
                    }
                    // Linux rv64 process entry ABI:
                    // a0=argc, a1=argv, a2=envp.
                    tf.x[10] = argc;
                    tf.x[11] = argv_ptr;
                    tf.x[12] = envp_ptr;
                    tf.sstatus = (1 << 5) | (1 << 13); // SPP=0, SPIE=1, FS=Initial
                    SyscallAction::Continue
                },
                Err(e) => SyscallAction::Return(syscall_error_return(e)),
            }
        },
        SyscallId::WAIT4 => {
            let ret = match process::sys_wait4_async(task, a0 as isize, a1, a2).await {
                Ok(pid) => pid as usize,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::FUTEX => {
            let ret = match sync::sys_futex_async(task, a0, a1 as u32, a2 as u32, a3).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::MOUNT => {
            let ret = match fs::sys_mount_async(task, a0, a1, a2, a3, a4).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::UMOUNT2 => {
            let ret = match fs::sys_umount2_async(task, a0, a1).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::MKDIRAT => {
            let ret = match fs::sys_mkdirat_async(task, a0 as isize, a1, a2).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::UNLINKAT => {
            let ret = match fs::sys_unlinkat_async(task, a0 as isize, a1, a2 as i32).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETDENTS64 => {
            let ret = match fs::sys_getdents64_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SYMLINKAT => {
            let ret = match fs::sys_symlinkat_async(task, a0, a1 as isize, a2).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::RENAMEAT2 => {
            let ret =
                match fs::sys_renameat2_async(task, a0 as isize, a1, a2 as isize, a3, a4).await {
                    Ok(()) => 0,
                    Err(e) => syscall_error_return(e),
                };
            SyscallAction::Return(ret)
        },
        SyscallId::UMASK => {
            // Stub: return previous umask (0o022), accept silently
            SyscallAction::Return(0o022)
        },
        // getrusage(who, usage_ptr) — stub: zero-fill the rusage struct.
        // Enough for benchmarks that call getrusage to measure user/sys time.
        SyscallId::GETRUSAGE => {
            let usage_ptr = a1;
            if usage_ptr != 0 {
                // struct rusage is 128 bytes on rv64 (two timeval + 14 longs)
                let zeros = [0u8; 128];
                let rc = unsafe {
                    crate::hal::copy_user_chunk(usage_ptr as *mut u8, zeros.as_ptr(), 128)
                };
                if rc != 0 {
                    SyscallAction::Return(syscall_error_return(Errno::Efault))
                } else {
                    SyscallAction::Return(0)
                }
            } else {
                SyscallAction::Return(syscall_error_return(Errno::Efault))
            }
        },
        SyscallId::FSTATAT => {
            let ret = match fs::sys_fstatat_async(task, a0 as isize, a1, a2, a3).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        // ---- Network syscalls ----
        SyscallId::SOCKET => {
            let ret = match net::sys_socket(task, a0, a1, a2) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::BIND => {
            let ret = match net::sys_bind(task, a0, a1, a2).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::LISTEN => {
            let ret = match net::sys_listen(task, a0, a1) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::ACCEPT => {
            let ret = match net::sys_accept(task, a0, a1, a2).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::CONNECT => {
            let ret = match net::sys_connect(task, a0, a1, a2).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETSOCKNAME => {
            let ret = match net::sys_getsockname(task, a0, a1, a2).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETPEERNAME => {
            let ret = match net::sys_getpeername(task, a0, a1, a2).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SENDTO => {
            let ret = match net::sys_sendto(task, a0, a1, a2, a3, a4, a5).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::RECVFROM => {
            let ret = match net::sys_recvfrom(task, a0, a1, a2, a3, a4, a5).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SETSOCKOPT => {
            let ret = match net::sys_setsockopt(task, a0, a1, a2, a3, a4) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::GETSOCKOPT => {
            let ret = match net::sys_getsockopt(task, a0, a1, a2, a3, a4) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SHUTDOWN => {
            let ret = match net::sys_shutdown(task, a0, a1) {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::SENDMSG => {
            let ret = match net::sys_sendmsg(task, a0, a1, a2).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::RECVMSG => {
            let ret = match net::sys_recvmsg(task, a0, a1, a2).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        SyscallId::ACCEPT4 => {
            let ret = match net::sys_accept4(task, a0, a1, a2, a3).await {
                Ok(v) => v,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        // set_robust_list — musl pthread init calls this; stub returning 0 is fine.
        SyscallId::SET_ROBUST_LIST => SyscallAction::Return(0),
        // clock_nanosleep(clockid, flags, req, rem) — forward to nanosleep logic.
        SyscallId::CLOCK_NANOSLEEP => {
            // a0=clockid, a1=flags, a2=req, a3=rem
            let ret = match sync::sys_nanosleep_async(task, a2, a3).await {
                Ok(()) => 0,
                Err(e) => syscall_error_return(e),
            };
            SyscallAction::Return(ret)
        },
        // sched_getscheduler — return SCHED_OTHER (0).
        SyscallId::SCHED_GETSCHEDULER => SyscallAction::Return(0),
        // sched_get_priority_max/min — SCHED_OTHER range is 0..0.
        SyscallId::SCHED_GET_PRIORITY_MAX => SyscallAction::Return(0),
        SyscallId::SCHED_GET_PRIORITY_MIN => SyscallAction::Return(0),
        // rseq — restartable sequences; not supported, ENOSYS is expected by musl.
        SyscallId::RSEQ => SyscallAction::Return(syscall_error_return(Errno::Enosys)),
        // riscv_flush_icache — no-op, our icache is coherent after trap return.
        SyscallId::RISCV_FLUSH_ICACHE => SyscallAction::Return(0),
        // prlimit64(pid, resource, new_rlim, old_rlim) — stub: if old_rlim != NULL,
        // fill with generous defaults; ignore new_rlim.
        SyscallId::PRLIMIT64 => {
            let old_rlim = a3;
            if old_rlim != 0 {
                // struct rlimit { rlim_cur, rlim_max } = 2 × u64
                // Return a large soft/hard limit (1GB / unlimited).
                let rlim = [1u64 << 30, u64::MAX];
                let rc = unsafe {
                    crate::hal::copy_user_chunk(
                        old_rlim as *mut u8,
                        rlim.as_ptr() as *const u8,
                        16,
                    )
                };
                if rc != 0 {
                    SyscallAction::Return(syscall_error_return(Errno::Efault))
                } else {
                    SyscallAction::Return(0)
                }
            } else {
                SyscallAction::Return(0)
            }
        },
        // getrandom(buf, buflen, flags) — fill buffer with pseudo-random bytes.
        SyscallId::GETRANDOM => {
            let buf = a0;
            let buflen = a1;
            if buf == 0 || buflen == 0 {
                SyscallAction::Return(0)
            } else {
                let len = buflen.min(256);
                let mut rand_buf = [0u8; 256];
                // Simple PRNG seeded from timer — good enough for musl stack guard.
                let mut seed = crate::hal::rv64::timer::read_time() as u64;
                for b in rand_buf[..len].iter_mut() {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    *b = (seed >> 33) as u8;
                }
                let rc = unsafe {
                    crate::hal::copy_user_chunk(buf as *mut u8, rand_buf.as_ptr(), len)
                };
                if rc != 0 {
                    SyscallAction::Return(syscall_error_return(Errno::Efault))
                } else {
                    SyscallAction::Return(len)
                }
            }
        },
        _ => {
            crate::klog!(
                syscall,
                error,
                "unimplemented {} (a0={:#x} a1={:#x} a2={:#x})",
                id,
                a0,
                a1,
                a2
            );
            SyscallAction::Return(syscall_error_return(Errno::Enosys))
        },
    };

    if let SyscallAction::Return(ret) = action {
        crate::klog!(syscall, debug, "pid={} {} -> {:#x}", task.pid, id, ret);
    }
    action
}
