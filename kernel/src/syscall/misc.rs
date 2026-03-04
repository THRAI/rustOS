//! Miscellaneous system calls.
//!
//! Implements uname and other utility syscalls.

use alloc::sync::Arc;
use hal_common::Errno;

use crate::proc::task::Task;

/// sys_uname: get system information.
pub fn sys_uname(task: &Arc<Task>, buf: usize) -> Result<(), Errno> {
    let _ = task;
    if buf == 0 {
        return Err(Errno::EFAULT);
    }
    // struct utsname: 6 fields × 65 bytes = 390 bytes
    const FIELD_LEN: usize = 65;
    let mut utsname = [0u8; FIELD_LEN * 6];

    fn write_field(buf: &mut [u8], offset: usize, s: &[u8]) {
        let end = s.len().min(64);
        buf[offset..offset + end].copy_from_slice(&s[..end]);
        // already zero-filled, NUL terminator is implicit
    }

    write_field(&mut utsname, FIELD_LEN * 0, b"FreeBSD"); // sysname
    write_field(&mut utsname, FIELD_LEN * 1, b"chronix"); // nodename
    write_field(&mut utsname, FIELD_LEN * 2, b"0.1.0"); // release
    write_field(&mut utsname, FIELD_LEN * 3, b"chronix 0.1.0"); // version
    write_field(&mut utsname, FIELD_LEN * 4, b"riscv64"); // machine
    write_field(&mut utsname, FIELD_LEN * 5, b"(none)"); // domainname

    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(buf as *mut u8, utsname.as_ptr(), utsname.len())
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}
