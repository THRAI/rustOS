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
}
