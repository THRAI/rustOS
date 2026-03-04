//! File system related system calls.

use crate::fs::delegate;
use crate::fs::fd_table::{FdFlags, FdTable, FileDescription, FileObject, OpenFlags};
use crate::fs::page_cache;
use crate::fs::vnode::VnodeType;
use hal_common::Errno;
use crate::mm::vm::fault::PageFaultAccessType;
use crate::proc::task::Task;
use crate::proc::user_copy::{copyinstr, fault_in_user_buffer};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal_common::PhysAddr;
use core::sync::atomic::Ordering;

const AT_FDCWD: isize = -100;
const PAGE_SIZE: usize = 4096;

/// Linux struct stat for rv64 (128 bytes).
#[repr(C)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad1: u64,
    st_size: i64,
    st_blksize: i32,
    __pad2: i32,
    st_blocks: i64,
    st_atime: i64,
    st_atime_nsec: i64,
    st_mtime: i64,
    st_mtime_nsec: i64,
    st_ctime: i64,
    st_ctime_nsec: i64,
    __unused: [i32; 2],
}

/// Resolve path, create `FileDescription`, and insert into fd table.
pub async fn open(
    fd_table: &hal_common::SpinMutex<FdTable>,
    path_str: &str,
    flags: OpenFlags,
) -> Result<u32, Errno> {
    if let Some(dev_name) = path_str.strip_prefix("/dev/") {
        let desc = crate::fs::devfs::open_device(dev_name, flags)?;
        let fd = fd_table.lock().insert(desc, FdFlags::empty())?;
        return Ok(fd);
    }

    let vnode = crate::fs::path::resolve(path_str).await?;
    let desc = FileDescription::new(FileObject::Vnode(vnode), flags);
    let fd = fd_table.lock().insert(desc, FdFlags::empty())?;
    Ok(fd)
}

/// Remove fd from table.
pub fn close(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32) -> Result<(), Errno> {
    let _desc = fd_table.lock().remove(fd).ok_or(Errno::EBADF)?;
    Ok(())
}

/// Get file metadata by fd.
pub fn stat(fd_table: &hal_common::SpinMutex<FdTable>, fd: u32) -> Result<(u64, u8), Errno> {
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
        FileObject::Device(_) => Ok((0, 3)),
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => Ok((0, 4)),
    }
}

/// Read from fd into kernel buffer (for kernel-level tests).
/// This is a simplified path for vnode reads only.
pub async fn read(
    fd_table: &hal_common::SpinMutex<FdTable>,
    fd: u32,
    buf: &mut [u8],
) -> Result<usize, Errno> {
    let (id, path, size, offset, desc) = {
        let tab = fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::EBADF)?;
        match &d.object {
            FileObject::Vnode(v) => (
                v.vnode_id(),
                alloc::string::String::from(v.path()),
                v.size(),
                d.offset.load(Ordering::Relaxed),
                Arc::clone(d),
            ),
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
        unsafe {
            let src_slice = pa.as_slice();
            buf[total..total + chunk].copy_from_slice(&src_slice[in_page..in_page + chunk]);
        }
        total += chunk;
    }

    desc.offset.store(offset + total as u64, Ordering::Relaxed);
    Ok(total)
}

/// Fetch a page from the page cache (kernel-side helper for `read`).
async fn page_cache_fetch(vnode_id: u64, path: &str, file_offset: u64) -> Result<PhysAddr, Errno> {
    use crate::fs::page_cache::LookupResult;

    let page_offset = file_offset / PAGE_SIZE as u64;
    loop {
        let noop = noop_waker();
        match page_cache::lookup(vnode_id, page_offset, &noop) {
            LookupResult::Hit(pa) => return Ok(pa),
            LookupResult::InitiateFetch => match delegate::fs_read_page(path, file_offset).await {
                Ok(pa_usize) => {
                    let pa = PhysAddr::new(pa_usize);
                    page_cache::complete(vnode_id, page_offset, pa);
                    return Ok(pa);
                }
                Err(_) => return Err(Errno::EIO),
            },
            LookupResult::WaitingOnFetch => {
                crate::executor::schedule::yield_now().await;
            }
        }
    }
}

fn noop_waker() -> core::task::Waker {
    use core::task::{RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone_fn(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
}

/// sys_lseek: reposition file offset.
pub fn sys_lseek(task: &Arc<Task>, fd: u32, offset: i64, whence: u32) -> Result<u64, Errno> {
    use crate::fs::fd_table::FileObject;
    use core::sync::atomic::Ordering;

    const SEEK_SET: u32 = 0;
    const SEEK_CUR: u32 = 1;
    const SEEK_END: u32 = 2;

    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::EBADF)?;

    // Pipes and devices are not seekable
    match &desc.object {
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => return Err(Errno::ESPIPE),
        FileObject::Device(_) => return Err(Errno::ESPIPE),
        FileObject::Vnode(_) => {}
    }

    let size = match &desc.object {
        FileObject::Vnode(v) => v.size(),
        _ => 0,
    };

    let cur = desc.offset.load(Ordering::Relaxed) as i64;
    let new_off = match whence {
        SEEK_SET => offset,
        SEEK_CUR => cur + offset,
        SEEK_END => size as i64 + offset,
        _ => return Err(Errno::EINVAL),
    };

    if new_off < 0 {
        return Err(Errno::EINVAL);
    }

    desc.offset.store(new_off as u64, Ordering::Relaxed);
    Ok(new_off as u64)
}

/// sys_fstat: write stat struct to user memory.
pub fn sys_fstat(task: &Arc<Task>, fd: u32, statbuf: usize) -> Result<(), Errno> {
    use crate::fs::fd_table::FileObject;

    if statbuf == 0 {
        return Err(Errno::EFAULT);
    }

    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::EBADF)?;

    let mut st = LinuxStat {
        st_dev: 0,
        st_ino: 0,
        st_mode: 0,
        st_nlink: 1,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad1: 0,
        st_size: 0,
        st_blksize: 4096,
        __pad2: 0,
        st_blocks: 0,
        st_atime: 0,
        st_atime_nsec: 0,
        st_mtime: 0,
        st_mtime_nsec: 0,
        st_ctime: 0,
        st_ctime_nsec: 0,
        __unused: [0; 2],
    };

    match &desc.object {
        FileObject::Vnode(v) => {
            let size = v.size();
            st.st_size = size as i64;
            st.st_blocks = ((size + 511) / 512) as i64;
            st.st_ino = v.vnode_id();
            // S_IFREG=0o100000 or S_IFDIR=0o040000
            use crate::fs::vnode::VnodeType;
            st.st_mode = match v.vtype() {
                VnodeType::Regular => 0o100644,
                VnodeType::Directory => 0o040755,
            };
        }
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => {
            st.st_mode = 0o010600; // S_IFIFO | rw
        }
        FileObject::Device(dk) => {
            use crate::fs::fd_table::DeviceKind;
            st.st_mode = 0o020666; // S_IFCHR | rw
            st.st_rdev = match dk {
                DeviceKind::Null => 0x0103,                                   // 1:3
                DeviceKind::Zero => 0x0105,                                   // 1:5
                DeviceKind::ConsoleRead | DeviceKind::ConsoleWrite => 0x0501, // 5:1
            };
        }
    }

    // Copy stat struct to user memory
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            statbuf as *mut u8,
            &st as *const LinuxStat as *const u8,
            core::mem::size_of::<LinuxStat>(),
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// sys_fstatat: stat a file by path (relative to dirfd).
pub async fn sys_fstatat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    statbuf: usize,
) -> Result<(), Errno> {
    if statbuf == 0 {
        return Err(Errno::EFAULT);
    }

    let raw_path = copyinstr(task, pathname_ptr, 256).await.ok_or(Errno::EFAULT)?;
    let path_str = absolutize_path(task, dirfd, &raw_path)?;

    // Resolve the path to a vnode
    let vnode = crate::fs::path::resolve(&path_str).await?;

    let mut st = LinuxStat {
        st_dev: 0,
        st_ino: 0,
        st_mode: 0,
        st_nlink: 1,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad1: 0,
        st_size: 0,
        st_blksize: 4096,
        __pad2: 0,
        st_blocks: 0,
        st_atime: 0,
        st_atime_nsec: 0,
        st_mtime: 0,
        st_mtime_nsec: 0,
        st_ctime: 0,
        st_ctime_nsec: 0,
        __unused: [0; 2],
    };

    st.st_ino = vnode.vnode_id();
    st.st_size = vnode.size() as i64;
    st.st_blocks = ((vnode.size() + 511) / 512) as i64;
    use crate::fs::vnode::VnodeType;
    st.st_mode = match vnode.vtype() {
        VnodeType::Regular => 0o100755, // executable
        VnodeType::Directory => 0o040755,
    };

    fault_in_user_buffer(
        task,
        statbuf,
        core::mem::size_of::<LinuxStat>(),
        PageFaultAccessType::WRITE,
    )
    .await;
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            statbuf as *mut u8,
            &st as *const LinuxStat as *const u8,
            core::mem::size_of::<LinuxStat>(),
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// sys_fcntl: file control operations.
pub fn sys_fcntl(task: &Arc<Task>, fd: u32, cmd: u32, _arg: usize) -> Result<usize, Errno> {
    const F_DUPFD: u32 = 0;
    const F_GETFD: u32 = 1;
    const F_SETFD: u32 = 2;
    const F_GETFL: u32 = 3;
    const F_SETFL: u32 = 4;
    const F_DUPFD_CLOEXEC: u32 = 1030;

    use crate::fs::fd_table::FdFlags;

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let mut tab = task.fd_table.lock();
            let desc = Arc::clone(tab.get(fd).ok_or(Errno::EBADF)?);
            let flags = if cmd == F_DUPFD_CLOEXEC {
                FdFlags::CLOEXEC
            } else {
                FdFlags::empty()
            };
            // Find lowest fd >= arg
            let new_fd = tab.insert(desc, flags)?;
            Ok(new_fd as usize)
        }
        F_GETFD => {
            let tab = task.fd_table.lock();
            let flags = tab.get_flags(fd).ok_or(Errno::EBADF)?;
            Ok(if flags.contains(FdFlags::CLOEXEC) {
                1
            } else {
                0
            })
        }
        F_SETFD => {
            // We only support CLOEXEC (bit 0)
            // For now, accept silently — FdTable doesn't have set_flags, so stub it
            let tab = task.fd_table.lock();
            let _ = tab.get(fd).ok_or(Errno::EBADF)?;
            Ok(0)
        }
        F_GETFL => {
            let tab = task.fd_table.lock();
            let desc = tab.get(fd).ok_or(Errno::EBADF)?;
            let mut fl: usize = 0;
            if desc.flags.read && desc.flags.write {
                fl = 2; // O_RDWR
            } else if desc.flags.write {
                fl = 1; // O_WRONLY
            }
            // O_RDONLY = 0
            Ok(fl)
        }
        F_SETFL => {
            // Accept silently — we don't support O_NONBLOCK/O_APPEND yet
            let tab = task.fd_table.lock();
            let _ = tab.get(fd).ok_or(Errno::EBADF)?;
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}

/// sys_chdir: change current working directory.
pub async fn sys_chdir_async(task: &Arc<Task>, pathname_ptr: usize) -> Result<(), Errno> {
    let raw_path = copyinstr(task, pathname_ptr, 256).await.ok_or(Errno::EFAULT)?;
    let path = absolutize_path(task, AT_FDCWD, &raw_path)?;
    let vnode = crate::fs::path::resolve(&path).await?;
    if vnode.vtype() != crate::fs::vnode::VnodeType::Directory {
        return Err(Errno::ENOTDIR);
    }
    *task.cwd.lock() = path;
    Ok(())
}

/// sys_getcwd: return current working directory.
pub fn sys_getcwd(
    task: &Arc<Task>,
    buf: usize,
    size: usize,
) -> Result<usize, Errno> {
    let cwd = task.cwd.lock().clone();
    let needed = cwd.len() + 1;
    if size < needed {
        return Err(Errno::ERANGE);
    }
    if buf == 0 {
        return Err(Errno::EINVAL);
    }
    let mut out = cwd.into_bytes();
    out.push(0);
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            buf as *mut u8, out.as_ptr(), out.len(),
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(buf)
}

/// sys_pipe2: create a pipe with optional flags.
pub fn sys_pipe2(task: &Arc<Task>, pipefd_ptr: usize, flags: usize) -> Result<(), Errno> {
    use crate::fs::fd_table::{FdFlags, FileDescription, FileObject, OpenFlags};
    use crate::fs::pipe::Pipe;

    let pipe = Pipe::new();
    let cloexec = (flags & 0o2000000) != 0;
    let fd_flags = if cloexec {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };

    let read_desc =
        FileDescription::new(FileObject::PipeRead(Arc::clone(&pipe)), OpenFlags::RDONLY);
    let write_desc = FileDescription::new(FileObject::PipeWrite(pipe), OpenFlags::WRONLY);

    let (read_fd, write_fd) = {
        let mut tab = task.fd_table.lock();
        let rfd = tab.insert(read_desc, fd_flags)?;
        let wfd = match tab.insert(write_desc, fd_flags) {
            Ok(fd) => fd,
            Err(e) => {
                tab.remove(rfd); // rollback
                return Err(e);
            }
        };
        (rfd, wfd)
    };

    // Write [read_fd, write_fd] to user memory
    if pipefd_ptr != 0 {
        let fds: [i32; 2] = [read_fd as i32, write_fd as i32];
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                pipefd_ptr as *mut u8,
                fds.as_ptr() as *const u8,
                8,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
    }

    Ok(())
}

/// sys_openat: resolve path and open file.
pub async fn sys_openat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    flags: usize,
) -> Result<u32, Errno> {
    // Read pathname from user memory using fault-safe copyinstr.
    let raw_path = copyinstr(task, pathname_ptr, 256).await.ok_or(Errno::EFAULT)?;
    let path_str = absolutize_path(task, dirfd, &raw_path)?;

    let open_flags = OpenFlags {
        read: true,
        write: (flags & 0x1) != 0 || (flags & 0x2) != 0,
    };

    open(&task.fd_table, &path_str, open_flags).await
}

/// sys_close: close a file descriptor.
pub fn sys_close(task: &Arc<Task>, fd: u32) -> Result<(), Errno> {
    close(&task.fd_table, fd)
}

/// Normalize a path to an absolute canonical form.
/// Collapses duplicate '/', '.' and '..' components.
pub fn normalize_absolute_path(path: &str) -> String {
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                let _ = comps.pop();
            }
            _ => comps.push(comp),
        }
    }

    if comps.is_empty() {
        return String::from("/");
    }

    let mut out = String::from("/");
    for (idx, comp) in comps.iter().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        out.push_str(comp);
    }
    out
}

/// Convert a user-provided path to an absolute path with cwd/dirfd semantics.
pub fn absolutize_path(task: &Arc<Task>, dirfd: isize, raw_path: &str) -> Result<String, Errno> {
    if raw_path.is_empty() {
        return Err(Errno::ENOENT);
    }
    if raw_path.starts_with('/') {
        return Ok(normalize_absolute_path(raw_path));
    }

    // Relative path from cwd.
    if dirfd == AT_FDCWD {
        let cwd = task.cwd.lock().clone();
        let mut combined = String::new();
        if cwd == "/" {
            combined.push('/');
            combined.push_str(raw_path);
        } else {
            combined.push_str(&cwd);
            combined.push('/');
            combined.push_str(raw_path);
        }
        return Ok(normalize_absolute_path(&combined));
    }

    // Relative path from directory fd.
    if dirfd >= 0 {
        let base = {
            let tab = task.fd_table.lock();
            let desc = tab.get(dirfd as u32).ok_or(Errno::EBADF)?;
            match &desc.object {
                crate::fs::fd_table::FileObject::Vnode(v) => {
                    if v.vtype() != crate::fs::vnode::VnodeType::Directory {
                        return Err(Errno::ENOTDIR);
                    }
                    String::from(v.path())
                }
                _ => return Err(Errno::ENOTDIR),
            }
        };
        let mut combined = String::new();
        if base == "/" {
            combined.push('/');
            combined.push_str(raw_path);
        } else {
            combined.push_str(&base);
            combined.push('/');
            combined.push_str(raw_path);
        }
        return Ok(normalize_absolute_path(&combined));
    }

    Err(Errno::EINVAL)
}
