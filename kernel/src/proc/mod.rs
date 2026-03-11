//! Process subsystem: Task lifecycle, fork, exec, exit, wait4, signals.

pub mod elf;
pub mod exec;
pub mod exit_wait;
pub mod fork;
pub mod pid;
pub mod signal;
pub mod syscall_result;
pub mod task;
pub mod user_copy;

pub use elf::{parse_elf_first_page, ExecContext};
#[cfg(feature = "exec-legacy")]
pub use exec::exec_with_args;
pub use exec::{do_execve, exec};
pub use exit_wait::{sys_exit, WaitChildFuture, WaitStatus};
pub use fork::fork;
pub use pid::alloc_pid;
pub use signal::{
    check_pending_signals, map_sigcode_page, register_task, sendsig, unregister_task, SigAction,
    SigFrame, SigSet, Signal, SignalState, MAX_SIG, SA_NOCLDSTOP, SA_NOCLDWAIT, SA_NODEFER,
    SA_ONSTACK, SA_RESETHAND, SA_RESTART, SA_RESTORER, SA_SIGINFO, SIGBUS, SIGCHLD, SIGCODE_VA,
    SIGCONT, SIGFPE, SIGFRAME_SIZE, SIGHUP, SIGILL, SIGINT, SIGKILL, SIGPIPE, SIGSEGV, SIGSTOP,
    SIGTERM, SIGUSR1, SIGUSR2, SIG_DFL, SIG_IGN,
};
pub(crate) use signal::{find_task_by_pid, for_each_task, kill_pgrp};
pub use syscall_result::SyscallResult;
pub use task::{Task, TaskState};
pub use user_copy::{copyin_argv, copyinstr, do_exit, fault_in_user_buffer};
