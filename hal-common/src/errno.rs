/// Kernel error codes (BSD-style)
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Errno {
    ENOMEM,
    EFAULT,
    EINVAL,
    ENOENT,
    EBADF,
    EAGAIN,
    EPERM,
    ENOSYS,
    ENOEXEC,
    ECHILD,
    EIO,
    EPIPE,
    ENOTTY,
    EMFILE,
    EEXIST,
    ESPIPE,
    EINTR,
    ETIMEDOUT,
    ESRCH,
}

impl Errno {
    /// Convert to negative errno value (Linux convention for syscall returns).
    pub fn as_i32(self) -> i32 {
        match self {
            Errno::EPERM => 1,
            Errno::ENOENT => 2,
            Errno::EIO => 5,
            Errno::EBADF => 9,
            Errno::ECHILD => 10,
            Errno::EAGAIN => 11,
            Errno::ENOMEM => 12,
            Errno::EFAULT => 14,
            Errno::EEXIST => 17,
            Errno::EINVAL => 22,
            Errno::EMFILE => 24,
            Errno::EPIPE => 32,
            Errno::ENOSYS => 38,
            Errno::ENOEXEC => 8,
            Errno::ENOTTY => 25,
            Errno::ESPIPE => 29,
            Errno::EINTR => 4,
            Errno::ETIMEDOUT => 110,
            Errno::ESRCH => 3,
        }
    }
}
