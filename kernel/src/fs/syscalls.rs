//! Filesystem syscalls: open, read, write, close, stat.
//!
//! These are async functions called from the syscall dispatch path.
//! They operate on the current task's FdTable and go through the
//! page cache and delegate for I/O.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use hal_common::{Errno, PhysAddr, PAGE_SIZE};

use super::delegate;
use super::fd_table::{FdFlags, FdTable, FileDescription, FileObject, OpenFlags};
use super::page_cache;
use super::vnode::{Vnode, VnodeType};

/// sys_open: resolve path, create FileDescription, insert into fd table.
pub async fn sys_open(fd_table: &hal_common::SpinMutex<FdTable>, path_str: &str, flags: OpenFlags) -> Result<u32, Errno> {
    // Check for /dev/ prefix
    if let Some(dev_name) = path_str.strip_prefix("/dev/") {
        let desc = super::devfs::open_device(dev_name, flags)?;
        let fd = fd_table.lock().insert(desc, FdFlags::empty())?;
        return Ok(fd);
    }

    let vnode = super::path::resolve(path_str).await?;
    let desc = FileDescription::new(FileObject::Vnode(vnode), flags);
    let fd = fd_table.lock().insert(desc, FdFlags::empty())?;
    Ok(fd)
}

/// sys_close: remove fd from table.
pub fn sys_close(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32) -> Result<(), Errno> {
    let desc = fd_table.lock().remove(fd).ok_or(Errno::EBADF)?;
    // Handle pipe close: notify the other end
    match &desc.object {
        FileObject::PipeRead(pipe) => pipe.close_read(),
        FileObject::PipeWrite(pipe) => pipe.close_write(),
        _ => {}
    }
    Ok(())
}

/// sys_stat: get file metadata by fd.
pub fn sys_stat(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32) -> Result<(u64, u8), Errno> {
    let table = fd_table.lock();
    let desc = table.get(fd).ok_or(Errno::EBADF)?;
    match &desc.object {
        FileObject::Vnode(vnode) => {
            let size = vnode.size();
            let vtype = match vnode.vtype() {
                VnodeType::Regular => 1u8,
                VnodeType::Directory => 2u8,
            };
            Ok((size, vtype))
        }
        FileObject::Device(_) => Ok((0, 3)), // char device
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => Ok((0, 4)), // pipe
    }
}
