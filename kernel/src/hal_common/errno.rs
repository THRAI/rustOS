//! Kernel error types and the canonical `KernelResult` alias.
//!
//! # Error Encoding Policy
//!
//! The kernel maintains a single Rust-level error type ([`Errno`]) and
//! converts to/from raw integers **only** at well-defined boundaries:
//!
//! | Boundary | Direction | Conversion |
//! |----------|-----------|------------|
//! | **Syscall return** | `Errno` → `a0` register | [`Errno::as_linux_ret`] (negative POSIX value) called **only** by `syscall_error_return()` in `syscall/mod.rs` |
//! | **lwext4 C FFI** | C `int` → `Errno` | `lwext4_err()` in `fs/ext4.rs` (accepts positive or negative C codes) |
//! | **copy_user asm** | asm `usize` → `Errno` | `uio.rs` compares against `Errno::Efault.as_i32()` (positive 14) |
//!
//! ## Rules
//!
//! 1. **Inside the Rust kernel, all fallible functions return
//!    [`KernelResult<T>`].** Never return a raw `usize` encoding an error.
//! 2. **[`Errno::as_i32`] returns positive POSIX values.** This is the
//!    canonical representation inside the kernel.
//! 3. **[`Errno::as_linux_ret`] returns negative values** following the
//!    Linux syscall convention (`-errno`). Only the syscall dispatcher
//!    should call this.
//! 4. **Domain-specific error enums** (e.g. `FaultError` in `mm/vm/`)
//!    are fine when they stay within their subsystem and never cross into
//!    syscall returns.

/// The canonical kernel error-return type.
///
/// Every fallible kernel function should return this instead of
/// `Result<T, Errno>` longhand.
pub type KernelResult<T> = core::result::Result<T, Errno>;

/// Kernel error codes (BSD-style, POSIX-numbered).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Errno {
    Enomem,
    Efault,
    Einval,
    Enoent,
    Ebadf,
    Eagain,
    Eperm,
    Enosys,
    Enoexec,
    Echild,
    Eio,
    Epipe,
    Enotty,
    Emfile,
    Eexist,
    Espipe,
    Eintr,
    Etimedout,
    Esrch,
    Erange,
    Enodev,
    Enotdir,
    Eisdir,
    Enotempty,
    Enametoolong,
}

impl Errno {
    /// Convert to **positive** errno value (POSIX numbering).
    ///
    /// This is the canonical representation used inside the kernel.
    /// For the Linux syscall-return convention (negative), use
    /// [`as_linux_ret`](Self::as_linux_ret) instead.
    pub fn as_i32(self) -> i32 {
        match self {
            Errno::Eperm => 1,
            Errno::Enoent => 2,
            Errno::Eio => 5,
            Errno::Ebadf => 9,
            Errno::Echild => 10,
            Errno::Eagain => 11,
            Errno::Enomem => 12,
            Errno::Efault => 14,
            Errno::Eexist => 17,
            Errno::Einval => 22,
            Errno::Emfile => 24,
            Errno::Epipe => 32,
            Errno::Enosys => 38,
            Errno::Enoexec => 8,
            Errno::Enotty => 25,
            Errno::Espipe => 29,
            Errno::Eintr => 4,
            Errno::Etimedout => 110,
            Errno::Esrch => 3,
            Errno::Erange => 34,
            Errno::Enodev => 19,
            Errno::Enotdir => 20,
            Errno::Eisdir => 21,
            Errno::Enotempty => 39,
            Errno::Enametoolong => 36,
        }
    }

    /// Return the **negative** errno value following the Linux syscall
    /// return convention: `-(positive_posix_code)`.
    ///
    /// This is the encoding placed into the user trap frame's `a0`
    /// register.  Only the syscall dispatcher (`syscall_error_return`)
    /// should call this method.
    #[inline]
    pub fn as_linux_ret(self) -> isize {
        -(self.as_i32() as isize)
    }
}
