//! Filesystem syscalls: open, read, write, close, stat, readdir.
//!
//! These are async functions called from the syscall dispatch path.
//! They operate on the current task's FdTable and go through the
//! page cache and delegate for I/O.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use hal_common::{Errno, PhysAddr, PAGE_SIZE};

use super::delegate;
use super::fd_table::{FdTable, OpenFile, OpenFlags};
use super::page_cache;
use super::path;
use super::vnode::{Vnode, VnodeType};

/// sys_open: resolve path, create OpenFile, insert into fd table.
pub async fn sys_open(fd_table: &hal_common::SpinMutex<FdTable>, path_str: &str, flags: OpenFlags) -> Result<u32, Errno> {
    let vnode = path::resolve(path_str).await?;
    let file = OpenFile::new(vnode, flags);
    let fd = fd_table.lock().insert(file);
    Ok(fd)
}

/// sys_read: read from fd into buffer, going through page cache.
pub async fn sys_read(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32, buf: &mut [u8]) -> Result<usize, Errno> {
    let (vnode, offset, file) = {
        let table = fd_table.lock();
        let f = table.get(fd).ok_or(Errno::EBADF)?;
        if !f.flags.read {
            return Err(Errno::EPERM);
        }
        (Arc::clone(&f.vnode), f.offset.load(Ordering::Relaxed), Arc::clone(f))
    };

    let file_size = vnode.size();
    if offset >= file_size {
        return Ok(0);
    }

    let to_read = buf.len().min((file_size - offset) as usize);
    let mut total = 0usize;
    let mut cur_offset = offset;

    while total < to_read {
        let page_offset = cur_offset / PAGE_SIZE as u64;
        let offset_in_page = (cur_offset % PAGE_SIZE as u64) as usize;

        // Try page cache probe first (no waker needed for simple read path)
        let pa = match page_cache::probe(vnode.vnode_id(), page_offset) {
            Some(pa) => pa,
            None => {
                // Fetch via delegate
                let page_byte_offset = page_offset * PAGE_SIZE as u64;
                match delegate::fs_read_page(vnode.path(), page_byte_offset).await {
                    Ok(pa_usize) => {
                        let pa = PhysAddr::new(pa_usize);
                        page_cache::complete(vnode.vnode_id(), page_offset, pa);
                        pa
                    }
                    Err(_) => return Err(Errno::EINVAL),
                }
            }
        };

        // Copy from page cache frame to user buffer
        let avail = PAGE_SIZE - offset_in_page;
        let chunk = (to_read - total).min(avail);
        let src = unsafe {
            core::slice::from_raw_parts((pa.as_usize() + offset_in_page) as *const u8, chunk)
        };
        buf[total..total + chunk].copy_from_slice(src);

        total += chunk;
        cur_offset += chunk as u64;
    }

    // Update file offset
    file.offset.store(cur_offset, Ordering::Relaxed);

    Ok(total)
}

/// sys_write: stub for Phase 3 (read-only filesystem).
pub async fn sys_write(_fd_table: &hal_common::SpinMutex<FdTable>, _fd: u32, _buf: &[u8]) -> Result<usize, Errno> {
    Err(Errno::EPERM) // Read-only filesystem in Phase 3
}

/// sys_close: remove fd from table.
pub fn sys_close(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32) -> Result<(), Errno> {
    fd_table.lock().remove(fd).ok_or(Errno::EBADF)?;
    Ok(())
}

/// sys_stat: get file metadata by fd.
pub fn sys_stat(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32) -> Result<(u64, u8), Errno> {
    let table = fd_table.lock();
    let f = table.get(fd).ok_or(Errno::EBADF)?;
    let size = f.vnode.size();
    let vtype = match f.vnode.vtype() {
        VnodeType::Regular => 1u8,
        VnodeType::Directory => 2u8,
    };
    Ok((size, vtype))
}
