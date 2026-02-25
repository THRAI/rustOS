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

/// sys_read: read from fd into kernel buffer (for kernel-level tests).
/// This is a simplified path for vnode reads only.
pub async fn sys_read(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32, buf: &mut [u8]) -> Result<usize, Errno> {
    let (id, path, size, offset, desc) = {
        let tab = fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::EBADF)?;
        match &d.object {
            FileObject::Vnode(v) => {
                let off = d.offset.load(Ordering::Relaxed);
                (v.vnode_id(), alloc::string::String::from(v.path()), v.size(), off, Arc::clone(d))
            }
            _ => return Err(Errno::ENOSYS),
        }
    };

    if offset >= size {
        return Ok(0);
    }
    let to_read = core::cmp::min(buf.len(), (size - offset) as usize);
    let mut total = 0usize;

    while total < to_read {
        let page_off = (offset + total as u64) / PAGE_SIZE as u64;
        let in_page = ((offset + total as u64) % PAGE_SIZE as u64) as usize;
        let chunk = core::cmp::min(PAGE_SIZE - in_page, to_read - total);

        let pa = page_cache_fetch(id, &path, page_off * PAGE_SIZE as u64).await?;
        let src = (pa.as_usize() + in_page) as *const u8;
        unsafe { core::ptr::copy_nonoverlapping(src, buf[total..].as_mut_ptr(), chunk); }
        total += chunk;
    }

    desc.offset.store(offset + total as u64, Ordering::Relaxed);
    Ok(total)
}

/// Fetch a page from the page cache (kernel-side helper for sys_read).
async fn page_cache_fetch(vnode_id: u64, path: &str, file_offset: u64) -> Result<PhysAddr, Errno> {
    use super::page_cache::LookupResult;

    let page_offset = file_offset / PAGE_SIZE as u64;
    loop {
        let noop = noop_waker();
        match page_cache::lookup(vnode_id, page_offset, &noop) {
            LookupResult::Hit(pa) => return Ok(pa),
            LookupResult::InitiateFetch => {
                match delegate::fs_read_page(path, file_offset).await {
                    Ok(pa_usize) => {
                        let pa = PhysAddr::new(pa_usize);
                        page_cache::complete(vnode_id, page_offset, pa);
                        return Ok(pa);
                    }
                    Err(_) => return Err(Errno::EIO),
                }
            }
            LookupResult::WaitingOnFetch => {
                // Yield and retry
                crate::executor::schedule::yield_now().await;
            }
        }
    }
}

fn noop_waker() -> core::task::Waker {
    use core::task::{RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone_fn(p: *const ()) -> RawWaker { RawWaker::new(p, &VTABLE) }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
}
