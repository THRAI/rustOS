//! Process subsystem: Task lifecycle, clone/fork, exec, exit, wait4, signals.

pub mod clone;
pub mod exec;
pub mod exit_wait;

pub mod pid;
pub mod signal;
pub mod syscall_result;
pub mod task;
pub mod user_copy;

pub use clone::{do_clone, CloneFlags, VforkDone};
pub use exec::do_execve;
pub use exit_wait::{WaitChildFuture, WaitStatus};
pub use pid::alloc_pid;
pub use signal::{
    check_pending_signals, unregister_task, SigAction, SigFrame, SigSet, Signal, SignalState,
    MAX_SIG, SA_NOCLDWAIT, SIGCHLD, SIGCODE_VA, SIGFRAME_SIZE, SIGKILL, SIGPIPE, SIGSEGV, SIGSTOP,
    SIG_DFL, SIG_IGN,
};
pub(crate) use signal::{find_task_by_pid, for_each_task, kill_pgrp, thread_group_has_live_peer};
pub use syscall_result::SyscallResult;
pub use task::{Task, TaskState};
pub use user_copy::{copyin_argv, copyinstr, do_exit, fault_in_user_buffer};
