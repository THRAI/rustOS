/// Kernel error codes (BSD-style)
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
}

impl Errno {
    /// Convert to negative errno value (Linux convention for syscall returns).
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
        }
    }
}
