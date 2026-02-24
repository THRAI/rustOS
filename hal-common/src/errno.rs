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
}

impl Errno {
    /// Convert to negative errno value (Linux convention for syscall returns).
    pub fn as_i32(self) -> i32 {
        match self {
            Errno::EPERM => 1,
            Errno::ENOENT => 2,
            Errno::EIO => 5,
            Errno::EAGAIN => 11,
            Errno::ENOMEM => 12,
            Errno::EFAULT => 14,
            Errno::EINVAL => 22,
            Errno::EBADF => 9,
            Errno::ENOSYS => 38,
            Errno::ENOEXEC => 8,
            Errno::ECHILD => 10,
        }
    }
}
