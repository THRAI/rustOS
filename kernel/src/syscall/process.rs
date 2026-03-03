//! Process management system calls.
//!
//! This module re-exports process syscalls from proc/syscalls.rs
//! to maintain a unified syscall interface.

// Re-export from existing proc/syscalls.rs
pub use crate::proc::syscalls::{
    sys_clone, sys_exit, sys_getegid, sys_geteuid, sys_getgid, sys_getpgid, sys_getpid,
    sys_getppid, sys_gettid, sys_getuid, sys_kill, sys_setpgid,
};
