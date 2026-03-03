//! Signal handling system calls.
//!
//! This module re-exports signal syscalls from proc/syscalls.rs
//! to maintain a unified syscall interface.

// Re-export from existing proc/syscalls.rs
pub use crate::proc::syscalls::{sys_sigaction, sys_sigprocmask, sys_sigreturn};
