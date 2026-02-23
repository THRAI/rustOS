//! SyscallResult: type-driven executor dispatch for syscall outcomes.
//!
//! The executor matches on this enum to decide whether to sret back to
//! userspace or drop the future (process exited).

/// Result of a syscall handler, consumed by the executor.
#[derive(Debug)]
pub enum SyscallResult {
    /// Normal return: write `value` to a0 and sret to userspace.
    ResumeUserspace(usize),
    /// Process called exit: executor drops the future, no sret.
    Terminated,
}
