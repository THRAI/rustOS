//! File system related system calls.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::Ordering,
    task::{Context, Poll},
};

use crate::{
    fs::{
        self, vnode_destroy_object, vnode_object, vnode_object_if_exists, FdFlags, FdTable,
        FileDescription, FileObject, OpenFlags, VnodeType,
    },
    hal_common::{Errno, VirtAddr},
    mm::{resolve_user_fault, uiomove, PageFaultAccessType, UioDir, VmObject},
    proc::{copyinstr, fault_in_user_buffer, Task},
};

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

#[inline]
fn stat_mode_from_type(file_type: u8) -> u32 {
    match file_type {
        2 => 0o040755, // directory
        7 => 0o120777, // symlink
        _ => 0o100644, // regular
    }
}

/// Resolve path, create `FileDescription`, and insert into fd table.
pub async fn open(
    fd_table: &crate::hal_common::SpinMutex<FdTable, 4>,
    path_str: &str,
    flags: OpenFlags,
    raw_flags: u32,
    fd_flags: FdFlags,
) -> Result<u32, Errno> {
    #[inline]
    fn normalize_delegate_open_flags(raw_flags: u32) -> u32 {
        // lwext4_rust::Ext4File::flags_to_cstring supports:
        // 0, 2, 0x241, 0x441, 0x242, 0x442.
        // Linux userspace may pass extra bits (e.g. O_NOCTTY/O_NONBLOCK/O_CLOEXEC),
        // so we canonicalize to the subset that lwext4 understands.
        const O_ACCMODE: u32 = 0x3;
        const O_WRONLY: u32 = 0x1;
        const O_RDWR: u32 = 0x2;
        const O_CREAT: u32 = 0x40;
        const O_TRUNC: u32 = 0x200;
        const O_APPEND: u32 = 0x400;

        let acc = raw_flags & O_ACCMODE;
        let creat = (raw_flags & O_CREAT) != 0;
        let trunc = (raw_flags & O_TRUNC) != 0;
        let append = (raw_flags & O_APPEND) != 0;

        if append {
            return if acc == O_RDWR {
                O_RDWR | O_CREAT | O_APPEND
            } else {
                O_WRONLY | O_CREAT | O_APPEND
            };
        }

        if creat {
            if trunc {
                return if acc == O_RDWR {
                    O_RDWR | O_CREAT | O_TRUNC
                } else {
                    O_WRONLY | O_CREAT | O_TRUNC
                };
            }
            // For create-without-trunc, use append mode to create file without clobbering
            // existing data. This matches `touch`-style behavior.
            return if acc == O_RDWR {
                O_RDWR | O_CREAT | O_APPEND
            } else {
                O_WRONLY | O_CREAT | O_APPEND
            };
        }

        match acc {
            O_RDWR => O_RDWR,
            O_WRONLY => O_RDWR, // lwext4 wrapper lacks a pure write-only mode
            _ => 0,             // O_RDONLY
        }
    }

    const O_CREAT: u32 = 0x40;
    const O_EXCL: u32 = 0x80;
    const O_TRUNC: u32 = 0x200;
    const O_DIRECTORY: u32 = 0x10000;

    if let Some(dev_name) = path_str.strip_prefix("/dev/") {
        let desc = crate::fs::open_device(dev_name, flags)?;
        let fd = fd_table.lock().insert(desc, fd_flags)?;
        return Ok(fd);
    }

    let create = (raw_flags & O_CREAT) != 0;
    let excl = (raw_flags & O_EXCL) != 0;
    let trunc = (raw_flags & O_TRUNC) != 0;

    // First resolve to distinguish "exists" from "create path".
    let first_resolve = crate::fs::resolve(path_str).await;
    if first_resolve.is_ok() && create && excl {
        return Err(Errno::Eexist);
    }

    let vnode = match first_resolve {
        Ok(v) => v,
        Err(e) => {
            if !create {
                return Err(e);
            }
            // Create by opening once with O_CREAT-compatible flags, then close.
            let create_flags = normalize_delegate_open_flags(raw_flags);
            let handle = crate::fs::fs_open_flags(path_str, create_flags)
                .await
                .map_err(|_| if excl { Errno::Eexist } else { Errno::Eio })?;
            let _ = crate::fs::fs_close(handle).await;
            crate::fs::resolve(path_str).await?
        },
    };

    // Handle O_DIRECTORY: verify it's a directory
    if (raw_flags & O_DIRECTORY) != 0 && vnode.vtype() != crate::fs::VnodeType::Directory {
        return Err(Errno::Enotdir);
    }

    // Handle O_TRUNC for regular files.
    // Under UBC, flush dirty pages before destroying the VmObject so that
    // data written but not yet synced is persisted up to the truncation point.
    if trunc && flags.write && vnode.vtype() == crate::fs::VnodeType::Regular {
        if let Some(obj_arc) = vnode_object_if_exists(vnode.vnode_id()) {
            let (dirty, pager) = {
                let obj = obj_arc.read();
                (
                    obj.collect_dirty_pages(),
                    obj.pager.as_ref().map(alloc::sync::Arc::clone),
                )
            };
            if let Some(pager) = pager {
                let file_size = vnode.size() as usize;
                for (idx, pa) in &dirty {
                    let offset = idx.to_bytes();
                    let len = if offset + PAGE_SIZE > file_size {
                        file_size.saturating_sub(offset)
                    } else {
                        PAGE_SIZE
                    };
                    if len > 0 {
                        let _ = pager.page_out(offset, *pa, len).await;
                    }
                }
            }
        }

        crate::fs::fs_truncate(path_str, 0)
            .await
            .map_err(|_| Errno::Eio)?;
        vnode_destroy_object(vnode.vnode_id());
        vnode.set_size(0);
    }

    let desc = FileDescription::new(FileObject::Vnode(vnode), flags);
    let fd = fd_table.lock().insert(desc, fd_flags)?;
    Ok(fd)
}

/// Remove fd from table.
pub fn close(fd_table: &crate::hal_common::SpinMutex<FdTable, 4>, fd: u32) -> Result<(), Errno> {
    let _desc = fd_table.lock().remove(fd).ok_or(Errno::Ebadf)?;
    Ok(())
}

/// Get file metadata by fd.
pub fn stat(
    fd_table: &crate::hal_common::SpinMutex<FdTable, 4>,
    fd: u32,
) -> Result<(u64, u8), Errno> {
    let table = fd_table.lock();
    let desc = table.get(fd).ok_or(Errno::Ebadf)?;
    match &desc.object {
        FileObject::Vnode(vnode) => {
            let size = vnode.size();
            let vtype = match vnode.vtype() {
                VnodeType::Regular => 1u8,
                VnodeType::Directory => 2u8,
            };
            Ok((size, vtype))
        },
        FileObject::Device(_) => Ok((0, 3)),
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => Ok((0, 4)),
    }
}

/// Read from fd into kernel buffer (for kernel-level tests).
/// This is a simplified path for vnode reads only.
#[allow(dead_code)]
pub async fn read(
    fd_table: &crate::hal_common::SpinMutex<FdTable, 4>,
    fd: u32,
    buf: &mut [u8],
) -> Result<usize, Errno> {
    let (vnode, size, offset, desc): (Arc<dyn crate::fs::Vnode>, u64, u64, Arc<FileDescription>) = {
        let tab = fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::Ebadf)?;
        match &d.object {
            FileObject::Vnode(v) => (
                Arc::clone(v),
                v.size(),
                d.offset.load(Ordering::Relaxed),
                Arc::clone(d),
            ),
            _ => return Err(Errno::Enosys),
        }
    };

    if offset >= size {
        return Ok(0);
    }
    let to_read = core::cmp::min(buf.len(), (size - offset) as usize);
    let mut total = 0usize;
    let obj = vnode_object(&*vnode);

    while total < to_read {
        let byte_off = offset + total as u64;
        let page_idx = crate::mm::vm::VObjIndex::from_bytes_floor(byte_off as usize);
        let in_page = (byte_off % PAGE_SIZE as u64) as usize;
        let chunk = core::cmp::min(PAGE_SIZE - in_page, to_read - total);

        let page = VmObject::fetch_page_async(Arc::clone(&obj), page_idx)
            .await
            .map_err(|_| Errno::Eio)?;
        let src_slice =
            unsafe { core::slice::from_raw_parts(page.as_usize() as *const u8, PAGE_SIZE) };
        buf[total..total + chunk].copy_from_slice(&src_slice[in_page..in_page + chunk]);
        total += chunk;
    }

    desc.offset.store(offset + total as u64, Ordering::Relaxed);
    Ok(total)
}

/// sys_lseek: reposition file offset.
pub fn sys_lseek(task: &Arc<Task>, fd: u32, offset: i64, whence: u32) -> Result<u64, Errno> {
    use core::sync::atomic::Ordering;

    use crate::fs::FileObject;

    const SEEK_SET: u32 = 0;
    const SEEK_CUR: u32 = 1;
    const SEEK_END: u32 = 2;

    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::Ebadf)?;

    // Pipes and devices are not seekable
    match &desc.object {
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => return Err(Errno::Espipe),
        FileObject::Device(_) => return Err(Errno::Espipe),
        FileObject::Vnode(_) => {},
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
        _ => return Err(Errno::Einval),
    };

    if new_off < 0 {
        return Err(Errno::Einval);
    }

    desc.offset.store(new_off as u64, Ordering::Relaxed);
    Ok(new_off as u64)
}

/// sys_fstat: write stat struct to user memory.
pub fn sys_fstat(task: &Arc<Task>, fd: u32, statbuf: usize) -> Result<(), Errno> {
    use crate::fs::FileObject;

    if statbuf == 0 {
        return Err(Errno::Efault);
    }

    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::Ebadf)?;

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
            let num_blocks = size.div_ceil(512) as i64;
            st.st_blocks = num_blocks;
            st.st_ino = v.vnode_id();
            use crate::fs::VnodeType;
            let ftype = match v.vtype() {
                VnodeType::Regular => 1u8,
                VnodeType::Directory => 2u8,
            };
            st.st_mode = stat_mode_from_type(ftype);
        },
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => {
            st.st_mode = 0o010600; // S_IFIFO | rw
        },
        FileObject::Device(dk) => {
            use crate::fs::DeviceKind;
            st.st_mode = 0o020666; // S_IFCHR | rw
            st.st_rdev = match dk {
                DeviceKind::Null => 0x0103,                                   // 1:3
                DeviceKind::Zero => 0x0105,                                   // 1:5
                DeviceKind::ConsoleRead | DeviceKind::ConsoleWrite => 0x0501, // 5:1
            };
        },
    }

    // Copy stat struct to user memory
    let rc = unsafe {
        crate::hal::copy_user_chunk(
            statbuf as *mut u8,
            &st as *const LinuxStat as *const u8,
            core::mem::size_of::<LinuxStat>(),
        )
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(())
}

/// sys_fstatat: stat a file by path (relative to dirfd).
pub async fn sys_fstatat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    statbuf: usize,
    flags: usize,
) -> Result<(), Errno> {
    const AT_SYMLINK_NOFOLLOW: usize = 0x100;
    const AT_EMPTY_PATH: usize = 0x1000;
    const AT_NO_AUTOMOUNT: usize = 0x800;

    if statbuf == 0 {
        return Err(Errno::Efault);
    }
    if (flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH | AT_NO_AUTOMOUNT)) != 0 {
        return Err(Errno::Einval);
    }

    // AT_EMPTY_PATH: stat by fd when pathname is NULL/empty.
    if pathname_ptr == 0 && (flags & AT_EMPTY_PATH) != 0 {
        if dirfd < 0 {
            return Err(Errno::Ebadf);
        }
        return sys_fstat(task, dirfd as u32, statbuf);
    }
    if pathname_ptr == 0 {
        return Err(Errno::Efault);
    }
    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    if raw_path.is_empty() && (flags & AT_EMPTY_PATH) != 0 {
        if dirfd < 0 {
            return Err(Errno::Ebadf);
        }
        return sys_fstat(task, dirfd as u32, statbuf);
    }
    if raw_path.is_empty() {
        return Err(Errno::Enoent);
    }
    let path_str = absolutize_path(task, dirfd, &raw_path)?;
    let (ino, ftype, size) = fs::fs_lookup(0, &path_str).await?;

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

    st.st_ino = ino as u64;
    st.st_size = size as i64;
    st.st_blocks = ((size + 511) / 512) as i64;
    st.st_mode = stat_mode_from_type(ftype);

    fault_in_user_buffer(
        task,
        statbuf,
        core::mem::size_of::<LinuxStat>(),
        PageFaultAccessType::WRITE,
    )
    .await;

    let rc = unsafe {
        crate::hal::copy_user_chunk(
            statbuf as *mut u8,
            &st as *const LinuxStat as *const u8,
            core::mem::size_of::<LinuxStat>(),
        )
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(())
}

/// sys_utimensat: update file timestamps.
///
/// Minimal behavior for userspace compatibility (e.g. BusyBox `touch`):
/// - resolve and validate target path exists;
/// - validate `timespec[2]` user pointer when non-null;
/// - currently ignore actual timestamp update and accepted flags.
pub async fn sys_utimensat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    times_ptr: usize,
    flags: usize,
) -> Result<(), Errno> {
    const AT_SYMLINK_NOFOLLOW: usize = 0x100;
    const AT_EMPTY_PATH: usize = 0x1000;
    const TIMESPEC_PAIR_SIZE: usize = 32; // struct timespec[2] on rv64

    // Accept only known flags for now.
    if (flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH)) != 0 {
        return Err(Errno::Einval);
    }

    // For now we only support pathname-based utimensat.
    if pathname_ptr == 0 {
        return Err(Errno::Einval);
    }

    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let path_str = absolutize_path(task, dirfd, &raw_path)?;
    let _ = crate::fs::resolve(&path_str).await?;

    // If user supplies timestamps, validate user memory accessibility.
    if times_ptr != 0 {
        fault_in_user_buffer(
            task,
            times_ptr,
            TIMESPEC_PAIR_SIZE,
            PageFaultAccessType::READ,
        )
        .await;
        let mut ts_buf = [0u8; TIMESPEC_PAIR_SIZE];
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                ts_buf.as_mut_ptr(),
                times_ptr as *const u8,
                TIMESPEC_PAIR_SIZE,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    Ok(())
}

/// sys_fcntl: file control operations.
pub fn sys_fcntl(task: &Arc<Task>, fd: u32, cmd: u32, arg: usize) -> Result<usize, Errno> {
    const F_DUPFD: u32 = 0;
    const F_GETFD: u32 = 1;
    const F_SETFD: u32 = 2;
    const F_GETFL: u32 = 3;
    const F_SETFL: u32 = 4;
    const F_DUPFD_CLOEXEC: u32 = 1030;
    const O_APPEND: usize = 0x0000_0400;
    const O_NONBLOCK: usize = 0x0000_0800;

    use crate::fs::FdFlags;

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let mut tab = task.fd_table.lock();
            let desc = Arc::clone(tab.get(fd).ok_or(Errno::Ebadf)?);
            let flags = if cmd == F_DUPFD_CLOEXEC {
                FdFlags::CLOEXEC
            } else {
                FdFlags::empty()
            };
            // Find lowest fd >= arg
            let new_fd = tab.insert_from(arg as u32, desc, flags)?;
            Ok(new_fd as usize)
        },
        F_GETFD => {
            let tab = task.fd_table.lock();
            let flags = tab.get_flags(fd).ok_or(Errno::Ebadf)?;
            Ok(if flags.contains(FdFlags::CLOEXEC) {
                1
            } else {
                0
            })
        },
        F_SETFD => {
            let mut tab = task.fd_table.lock();
            let _ = tab.get(fd).ok_or(Errno::Ebadf)?;
            let new_flags = if (arg & 1) != 0 {
                FdFlags::CLOEXEC
            } else {
                FdFlags::empty()
            };
            tab.set_flags(fd, new_flags)?;
            Ok(0)
        },
        F_GETFL => {
            let tab = task.fd_table.lock();
            let desc = tab.get(fd).ok_or(Errno::Ebadf)?;
            let mut fl: usize = 0;
            if desc.flags.read && desc.flags.write {
                fl = 2; // O_RDWR
            } else if desc.flags.write {
                fl = 1; // O_WRONLY
            }
            // O_RDONLY = 0
            let status = desc.get_status_flags() as usize;
            fl |= status & (O_APPEND | O_NONBLOCK);
            Ok(fl)
        },
        F_SETFL => {
            let tab = task.fd_table.lock();
            let desc = tab.get(fd).ok_or(Errno::Ebadf)?;
            let settable_mask = (O_APPEND | O_NONBLOCK) as u32;
            let cur = desc.get_status_flags();
            let next = (cur & !settable_mask) | ((arg as u32) & settable_mask);
            desc.set_status_flags(next);
            Ok(0)
        },
        _ => Err(Errno::Einval),
    }
}

/// sys_chdir: change current working directory.
pub async fn sys_chdir_async(task: &Arc<Task>, pathname_ptr: usize) -> Result<(), Errno> {
    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let path = absolutize_path(task, AT_FDCWD, &raw_path)?;
    let vnode = crate::fs::resolve(&path).await?;
    if vnode.vtype() != crate::fs::VnodeType::Directory {
        return Err(Errno::Enotdir);
    }
    *task.cwd.lock() = path;
    Ok(())
}

/// sys_getcwd: return current working directory.
pub fn sys_getcwd(task: &Arc<Task>, buf: usize, size: usize) -> Result<usize, Errno> {
    let cwd = task.cwd.lock().clone();
    let needed = cwd.len() + 1;
    if size < needed {
        return Err(Errno::Erange);
    }
    if buf == 0 {
        return Err(Errno::Einval);
    }
    let mut out = cwd.into_bytes();
    out.push(0);
    let rc = unsafe { crate::hal::copy_user_chunk(buf as *mut u8, out.as_ptr(), out.len()) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(buf)
}

/// sys_mount: minimal mount lifecycle support for oscomp/basic.
///
/// Current model:
/// 1. Validate userspace pointers/strings.
/// 2. Resolve and validate mount target directory in current namespace.
/// 3. Register target in a lightweight mount table.
/// 4. Keep existing single-root ext4 backend unchanged.
pub async fn sys_mount_async(
    task: &Arc<Task>,
    source_ptr: usize,
    target_ptr: usize,
    fstype_ptr: usize,
    flags: usize,
    data_ptr: usize,
) -> Result<(), Errno> {
    if source_ptr == 0 || target_ptr == 0 || fstype_ptr == 0 {
        return Err(Errno::Efault);
    }

    let source = copyinstr(task, source_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let raw_target = copyinstr(task, target_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let fstype = copyinstr(task, fstype_ptr, 64).await.ok_or(Errno::Efault)?;

    if data_ptr != 0 {
        let _ = copyinstr(task, data_ptr, 256).await.ok_or(Errno::Efault)?;
    }

    let target = absolutize_path(task, AT_FDCWD, &raw_target)?;
    let target_vnode = crate::fs::resolve(&target)
        .await
        .map_err(|_| Errno::Enoent)?;
    if target_vnode.vtype() != crate::fs::VnodeType::Directory {
        return Err(Errno::Enotdir);
    }
    crate::fs::register_mount(&source, &target, &fstype, flags)
}

/// sys_umount2: minimal unmount support paired with `sys_mount_async`.
pub async fn sys_umount2_async(
    task: &Arc<Task>,
    target_ptr: usize,
    flags: usize,
) -> Result<(), Errno> {
    const MNT_FORCE: usize = 0x0001;
    const MNT_DETACH: usize = 0x0002;
    const MNT_EXPIRE: usize = 0x0004;
    const UMOUNT_NOFOLLOW: usize = 0x0008;

    if target_ptr == 0 {
        return Err(Errno::Efault);
    }

    let known = MNT_FORCE | MNT_DETACH | MNT_EXPIRE | UMOUNT_NOFOLLOW;
    if (flags & !known) != 0 {
        return Err(Errno::Einval);
    }
    if (flags & MNT_EXPIRE) != 0 && ((flags & MNT_FORCE) != 0 || (flags & MNT_DETACH) != 0) {
        return Err(Errno::Einval);
    }

    let raw_target = copyinstr(task, target_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let target = absolutize_path(task, AT_FDCWD, &raw_target)?;
    if target == "/" {
        return Err(Errno::Einval);
    }
    crate::fs::unregister_mount(&target)
}

/// sys_linkat: create hard link.
pub async fn sys_linkat_async(
    task: &Arc<Task>,
    olddirfd: isize,
    oldpath_ptr: usize,
    newdirfd: isize,
    newpath_ptr: usize,
    flags: i32,
) -> Result<(), Errno> {
    const AT_SYMLINK_FOLLOW: i32 = 0x400;
    if (flags & !AT_SYMLINK_FOLLOW) != 0 {
        return Err(Errno::Einval);
    }
    let old_raw = copyinstr(task, oldpath_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let new_raw = copyinstr(task, newpath_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let old_path = absolutize_path(task, olddirfd, &old_raw)?;
    let new_path = absolutize_path(task, newdirfd, &new_raw)?;
    // Minimal cross-mount guard. Linux reports EXDEV; use EINVAL until EXDEV exists.
    if !crate::fs::same_mount_domain(&old_path, &new_path) {
        return Err(Errno::Einval);
    }
    fs::fs_link(&old_path, &new_path).await?;
    Ok(())
}

/// sys_renameat2: rename/move path.
pub async fn sys_renameat2_async(
    task: &Arc<Task>,
    olddirfd: isize,
    oldpath_ptr: usize,
    newdirfd: isize,
    newpath_ptr: usize,
    flags: usize,
) -> Result<(), Errno> {
    // Minimal implementation: only support Linux default behavior.
    if flags != 0 {
        return Err(Errno::Einval);
    }
    let old_raw = copyinstr(task, oldpath_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let new_raw = copyinstr(task, newpath_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let old_path = absolutize_path(task, olddirfd, &old_raw)?;
    let new_path = absolutize_path(task, newdirfd, &new_raw)?;
    // Minimal cross-mount guard. Linux reports EXDEV; use EINVAL until EXDEV exists.
    if !crate::fs::same_mount_domain(&old_path, &new_path) {
        return Err(Errno::Einval);
    }
    fs::fs_rename(&old_path, &new_path).await?;
    Ok(())
}

/// sys_readlinkat: read symbolic link target.
pub async fn sys_readlinkat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    buf_ptr: usize,
    bufsiz: usize,
) -> Result<usize, Errno> {
    if bufsiz == 0 {
        return Err(Errno::Einval);
    }
    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let path = absolutize_path(task, dirfd, &raw_path)?;
    let (mut n, data) = fs::fs_readlink(&path).await?;
    if n > 0 && data[n - 1] == 0 {
        n -= 1;
    }
    let copy_len = core::cmp::min(n, bufsiz);
    fault_in_user_buffer(task, buf_ptr, copy_len, PageFaultAccessType::WRITE).await;
    let rc = unsafe { crate::hal::copy_user_chunk(buf_ptr as *mut u8, data.as_ptr(), copy_len) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    Ok(copy_len)
}

/// sys_faccessat: access check (minimal).
pub async fn sys_faccessat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    mode: i32,
    flags: i32,
) -> Result<(), Errno> {
    const R_OK: i32 = 4;
    const W_OK: i32 = 2;
    const X_OK: i32 = 1;
    const F_OK: i32 = 0;
    const AT_EACCESS: i32 = 0x200;
    const AT_SYMLINK_NOFOLLOW: i32 = 0x100;

    if mode != F_OK && (mode & !(R_OK | W_OK | X_OK)) != 0 {
        return Err(Errno::Einval);
    }
    if (flags & !(AT_EACCESS | AT_SYMLINK_NOFOLLOW)) != 0 {
        return Err(Errno::Einval);
    }

    if pathname_ptr == 0 {
        return Err(Errno::Efault);
    }
    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    if raw_path.is_empty() {
        return Err(Errno::Enoent);
    }
    let path = absolutize_path(task, dirfd, &raw_path)?;
    let _ = fs::fs_lookup(0, &path).await?;
    Ok(())
}

/// sys_ftruncate: truncate regular file by fd.
///
/// Under UBC, dirty pages in the truncated range are flushed before the
/// on-disk truncation so data written but not yet synced is not lost.
///
/// NOTE: Concurrent write + truncate on the same fd from different threads
/// is undefined behavior per POSIX (requires flock for synchronization).
/// This implementation does not serialize them.
pub async fn sys_ftruncate_async(task: &Arc<Task>, fd: u32, len: u64) -> Result<(), Errno> {
    use crate::fs::FileObject;
    let desc = {
        let tab = task.fd_table.lock();
        Arc::clone(tab.get(fd).ok_or(Errno::Ebadf)?)
    };
    let (path, vnode_id) = match &desc.object {
        FileObject::Vnode(v) => (String::from(v.path()), v.vnode_id()),
        _ => return Err(Errno::Einval),
    };

    // Flush dirty pages in the range being truncated before disk truncation.
    if let Some(obj_arc) = vnode_object_if_exists(vnode_id) {
        let (dirty_in_range, pager) = {
            let obj = obj_arc.read();
            let trunc_idx = crate::mm::vm::VObjIndex::from_bytes_ceil(len as usize);
            let dirty: alloc::vec::Vec<_> = obj
                .pages_with_index()
                .filter(|(idx, pr)| *idx >= trunc_idx && pr.meta().is_dirty())
                .map(|(idx, pr)| (idx, pr.phys()))
                .collect();
            (dirty, obj.pager.as_ref().map(alloc::sync::Arc::clone))
        };
        // VmObject lock released before I/O

        if let Some(pager) = pager {
            let file_size = len as usize; // truncation target is the authoritative size
            for (idx, pa) in &dirty_in_range {
                let offset = idx.to_bytes();
                let write_len = if offset + PAGE_SIZE > file_size {
                    file_size.saturating_sub(offset)
                } else {
                    PAGE_SIZE
                };
                if write_len > 0 {
                    let _ = pager.page_out(offset, *pa, write_len).await;
                }
            }
        }
    }

    // Update VmObject size + truncate cached pages BEFORE on-disk truncation.
    // This prevents the page daemon from racing: if it flushes between
    // fs_truncate and set_size, it would use the old (larger) size to compute
    // write lengths, potentially re-extending the file on disk.
    if let FileObject::Vnode(v) = &desc.object {
        v.set_size(len);
    }

    fs::fs_truncate(&path, len).await?;

    let cur = desc.offset.load(Ordering::Relaxed);
    if cur > len {
        desc.offset.store(len, Ordering::Relaxed);
    }
    Ok(())
}

/// sys_fsync: flush data and metadata.
///
/// Two-phase flush under UBC:
/// 1. Flush dirty VmObject pages through pager → delegate → lwext4
/// 2. Flush lwext4's internal block cache to VirtIO disk
pub async fn sys_fsync_async(task: &Arc<Task>, fd: u32) -> Result<(), Errno> {
    use crate::fs::FileObject;
    let (path, vnode_id) = {
        let tab = task.fd_table.lock();
        let desc = tab.get(fd).ok_or(Errno::Ebadf)?;
        match &desc.object {
            FileObject::Vnode(v) => (String::from(v.path()), v.vnode_id()),
            _ => return Ok(()),
        }
    };

    // Phase 1: Flush dirty VmObject pages for this vnode
    crate::mm::vm::page_daemon::flush_vnode_dirty(vnode_id).await;

    // Phase 2: Flush lwext4's internal block cache to disk
    crate::fs::fs_cache_flush(&path).await
}

/// sys_fdatasync: flush data (same as fsync for now).
pub async fn sys_fdatasync_async(task: &Arc<Task>, fd: u32) -> Result<(), Errno> {
    sys_fsync_async(task, fd).await
}

/// sys_sync: flush all filesystem cache.
pub async fn sys_sync_async() -> Result<(), Errno> {
    // Phase 1: Flush all dirty VmObject pages
    crate::mm::vm::page_daemon::flush_all_dirty().await;
    // Phase 2: Flush lwext4 block cache
    crate::fs::fs_cache_flush("/").await
}

/// sys_pipe2: create a pipe with optional flags.
pub fn sys_pipe2(task: &Arc<Task>, pipefd_ptr: usize, flags: usize) -> Result<(), Errno> {
    use crate::fs::{FdFlags, FileDescription, FileObject, OpenFlags, Pipe};

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
            },
        };
        (rfd, wfd)
    };

    // Write [read_fd, write_fd] to user memory
    if pipefd_ptr != 0 {
        let fds: [i32; 2] = [read_fd as i32, write_fd as i32];
        let rc = unsafe {
            crate::hal::copy_user_chunk(pipefd_ptr as *mut u8, fds.as_ptr() as *const u8, 8)
        };
        if rc != 0 {
            return Err(Errno::Efault);
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
    // Open flag constants
    const O_RDONLY: usize = 0;
    const O_WRONLY: usize = 1;
    const O_RDWR: usize = 2;
    const O_TRUNC: usize = 0x200;
    const O_APPEND: usize = 0x400;
    const O_CLOEXEC: usize = 0x80000;

    // Read pathname from user memory using fault-safe copyinstr.
    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let path_str = absolutize_path(task, dirfd, &raw_path)?;

    // Parse access mode
    let access_mode = flags & 0x3;
    if access_mode > O_RDWR {
        return Err(Errno::Einval);
    }
    if (flags & O_TRUNC) != 0 && access_mode == O_RDONLY {
        return Err(Errno::Einval);
    }
    let open_flags = OpenFlags {
        read: access_mode == O_RDONLY || access_mode == O_RDWR,
        write: access_mode == O_WRONLY || access_mode == O_RDWR,
        append: (flags & O_APPEND) != 0,
    };
    let fd_flags = if (flags & O_CLOEXEC) != 0 {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };

    // Pass full flags to delegate for O_CREAT/O_TRUNC handling
    let delegate_flags = flags as u32;

    open(
        &task.fd_table,
        &path_str,
        open_flags,
        delegate_flags,
        fd_flags,
    )
    .await
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
            "" | "." => {},
            ".." => {
                let _ = comps.pop();
            },
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
        return Err(Errno::Enoent);
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
            let desc = tab.get(dirfd as u32).ok_or(Errno::Ebadf)?;
            match &desc.object {
                crate::fs::FileObject::Vnode(v) => {
                    if v.vtype() != crate::fs::VnodeType::Directory {
                        return Err(Errno::Enotdir);
                    }
                    String::from(v.path())
                },
                _ => return Err(Errno::Enotdir),
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

    Err(Errno::Einval)
}

/// sys_read_async: read from file descriptor.
pub async fn sys_read_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::fs::{DeviceKind, FileObject};

    if len == 0 {
        return Ok(0);
    }

    enum ReadSource {
        Vnode {
            vnode: Arc<dyn crate::fs::Vnode>,
            size: u64,
            offset: u64,
        },
        PipeRead(alloc::sync::Arc<crate::fs::Pipe>),
        DevNull,
        DevZero,
        DevConsole,
    }

    let (source, desc) = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::Ebadf)?;
        if !d.flags.read {
            return Err(Errno::Eperm);
        }
        let src = match &d.object {
            FileObject::Vnode(v) => ReadSource::Vnode {
                vnode: Arc::clone(v),
                size: v.size(),
                offset: d.offset.load(core::sync::atomic::Ordering::Relaxed),
            },
            FileObject::PipeRead(p) => ReadSource::PipeRead(Arc::clone(p)),
            FileObject::PipeWrite(_) => return Err(Errno::Ebadf),
            FileObject::Device(DeviceKind::Null) => ReadSource::DevNull,
            FileObject::Device(DeviceKind::Zero) => ReadSource::DevZero,
            FileObject::Device(DeviceKind::ConsoleRead) => ReadSource::DevConsole,
            FileObject::Device(DeviceKind::ConsoleWrite) => return Err(Errno::Ebadf),
        };
        (src, Arc::clone(d))
    };

    match source {
        ReadSource::DevNull => Ok(0),
        ReadSource::DevZero => {
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::WRITE).await;
            let zeros = alloc::vec![0u8; len];
            let rc =
                unsafe { crate::hal::copy_user_chunk(user_buf as *mut u8, zeros.as_ptr(), len) };
            if rc != 0 {
                Err(Errno::Efault)
            } else {
                Ok(len)
            }
        },
        ReadSource::DevConsole => {
            ConsoleReadFuture {
                task,
                user_buf,
                len,
            }
            .await
        },
        ReadSource::PipeRead(pipe) => {
            PipeReadFuture {
                pipe,
                task,
                user_buf,
                len,
            }
            .await
        },
        ReadSource::Vnode {
            vnode,
            size,
            mut offset,
        } => {
            if offset >= size {
                return Ok(0);
            }
            let to_read = core::cmp::min(len, (size - offset) as usize);
            let mut total = 0usize;
            let obj = vnode_object(&*vnode);

            while total < to_read {
                let page_idx = crate::mm::vm::VObjIndex::from_bytes_floor(offset as usize);
                let offset_in_page = (offset % PAGE_SIZE as u64) as usize;
                let chunk = core::cmp::min(PAGE_SIZE - offset_in_page, to_read - total);

                let page = VmObject::fetch_page_async(Arc::clone(&obj), page_idx)
                    .await
                    .map_err(|_| Errno::Eio)?;

                let kern = (page.as_usize() + offset_in_page) as *mut u8;
                let user = (user_buf + total) as *mut u8;

                match uiomove(kern, user, chunk, UioDir::CopyOut) {
                    Ok(result) => {
                        total += result.done;
                        offset += result.done as u64;
                    },
                    Err(Errno::Efault) => {
                        resolve_user_fault(
                            task,
                            VirtAddr::new(user_buf + total),
                            PageFaultAccessType::WRITE,
                        )
                        .await?;
                    },
                    Err(e) => return Err(e),
                }
            }

            desc.offset
                .store(offset, core::sync::atomic::Ordering::Relaxed);
            Ok(total)
        },
    }
}

/// sys_write_async: write to file descriptor.
pub async fn sys_write_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::fs::{DeviceKind, FileObject};

    if len == 0 {
        return Ok(0);
    }

    enum WriteTarget {
        DevNull,
        DevConsole,
        PipeWrite(alloc::sync::Arc<crate::fs::Pipe>),
        Vnode {
            path: String,
            offset: u64,
            append: bool,
        },
    }

    let (target, desc) = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::Ebadf)?;
        if !d.flags.write {
            return Err(Errno::Eperm);
        }
        let tgt = match &d.object {
            FileObject::Device(DeviceKind::Null) => WriteTarget::DevNull,
            FileObject::Device(DeviceKind::Zero) => WriteTarget::DevNull,
            FileObject::Device(DeviceKind::ConsoleWrite | DeviceKind::ConsoleRead) => {
                WriteTarget::DevConsole
            },
            FileObject::PipeWrite(p) => WriteTarget::PipeWrite(Arc::clone(p)),
            FileObject::PipeRead(_) => return Err(Errno::Ebadf),
            FileObject::Vnode(v) => WriteTarget::Vnode {
                path: String::from(v.path()),
                offset: d.offset.load(core::sync::atomic::Ordering::Relaxed),
                append: d.is_append(),
            },
        };
        (tgt, Arc::clone(d))
    };

    match target {
        WriteTarget::DevNull => Ok(len),
        WriteTarget::DevConsole => {
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::READ).await;
            let mut kbuf = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::copy_user_chunk(kbuf.as_mut_ptr(), user_buf as *const u8, len)
            };
            if rc != 0 {
                return Err(Errno::Efault);
            }
            // Use atomic batch write to prevent output interleaving
            crate::console::putchars(&kbuf);
            Ok(len)
        },
        WriteTarget::PipeWrite(pipe) => {
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::READ).await;
            let mut kbuf = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::copy_user_chunk(kbuf.as_mut_ptr(), user_buf as *const u8, len)
            };
            if rc != 0 {
                return Err(Errno::Efault);
            }
            let result = PipeWriteFuture {
                pipe,
                task,
                data: kbuf,
                written: 0,
            }
            .await;
            if let Err(Errno::Epipe) = &result {
                task.signals.post_signal(crate::proc::SIGPIPE);
            }
            result
        },
        WriteTarget::Vnode {
            path: _,
            mut offset,
            append,
        } => {
            // Handle O_APPEND: use vnode's authoritative size (local atomic, no delegate round-trip)
            let vnode = match &desc.object {
                FileObject::Vnode(v) => Arc::clone(v),
                _ => unreachable!(),
            };
            if append {
                offset = vnode.size();
            }

            // --- UBC unified write path ---
            // Data goes into VmObject pages; disk I/O is deferred to the page daemon / fsync.
            let obj = vnode_object(&*vnode);
            let to_write = len;
            let mut total = 0usize;

            // Extend VmObject size if writing past current EOF, so fetch_page_async
            // can allocate pages in the extended region.
            {
                let new_end = (offset as usize) + to_write;
                let cur_size = obj.read().size();
                if new_end > cur_size {
                    obj.write().set_size(new_end);
                }
            }

            while total < to_write {
                let byte_off = (offset as usize) + total;
                let page_idx = crate::mm::vm::VObjIndex::from_bytes_floor(byte_off);
                let offset_in_page = byte_off % PAGE_SIZE;
                let chunk = core::cmp::min(PAGE_SIZE - offset_in_page, to_write - total);

                // 1. Fetch page into VmObject (page_in from disk if not cached)
                let page = VmObject::fetch_page_async(Arc::clone(&obj), page_idx)
                    .await
                    .map_err(|_| Errno::Eio)?;

                // 2. Copy user data INTO the VmObject page
                let kern = (page.as_usize() + offset_in_page) as *mut u8;
                let user = (user_buf + total) as *mut u8;

                match uiomove(kern, user, chunk, UioDir::CopyIn) {
                    Ok(result) => {
                        total += result.done;
                    },
                    Err(Errno::Efault) => {
                        // Demand-page user buffer, retry this chunk
                        resolve_user_fault(
                            task,
                            VirtAddr::new(user_buf + total),
                            PageFaultAccessType::READ,
                        )
                        .await?;
                        continue;
                    },
                    Err(e) => {
                        if total > 0 {
                            break;
                        } // short write
                        return Err(e);
                    },
                }

                // 3. Mark page dirty and update dirty page counter
                if let Some(meta) = crate::mm::get_frame_meta(page) {
                    meta.set_dirty();
                    crate::mm::vm::page_daemon::maybe_wake_page_daemon();
                }
            }

            // 4. Bump object dirty generation (AtomicU32 — read lock is fine)
            obj.read().bump_generation();

            // 6. Extend file size if needed
            let new_end = offset + total as u64;
            if new_end > vnode.size() {
                vnode.set_size(new_end);
            }

            // 7. Update file offset
            desc.offset
                .store(offset + total as u64, core::sync::atomic::Ordering::Relaxed);

            Ok(total)
        },
    }
}

/// sys_readv_async: read into multiple buffers.
pub async fn sys_readv_async(
    task: &Arc<Task>,
    fd: u32,
    iov_ptr: usize,
    iovcnt: usize,
) -> Result<usize, Errno> {
    if iovcnt > 1024 {
        return Err(Errno::Einval);
    }
    if iovcnt == 0 {
        return Ok(0);
    }

    let iov_size = iovcnt * 16;
    fault_in_user_buffer(task, iov_ptr, iov_size, PageFaultAccessType::READ).await;
    let mut iov_buf = alloc::vec![0u8; iov_size];
    let rc = unsafe {
        crate::hal::copy_user_chunk(iov_buf.as_mut_ptr(), iov_ptr as *const u8, iov_size)
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }

    let mut total_read = 0;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_buf[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_buf[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }

        match sys_read_async(task, fd, base, len).await {
            Ok(0) => break,
            Ok(n) => {
                total_read += n;
                if n < len {
                    break;
                }
            },
            Err(e) => {
                if total_read > 0 {
                    return Ok(total_read);
                }
                return Err(e);
            },
        }
    }
    Ok(total_read)
}

/// sys_writev_async: write from multiple buffers.
pub async fn sys_writev_async(
    task: &Arc<Task>,
    fd: u32,
    iov_ptr: usize,
    iovcnt: usize,
) -> Result<usize, Errno> {
    if iovcnt == 0 {
        return Ok(0);
    }
    if iovcnt > 1024 {
        return Err(Errno::Einval);
    }

    let iov_size = iovcnt * 16;
    fault_in_user_buffer(task, iov_ptr, iov_size, PageFaultAccessType::READ).await;
    let mut iov_buf = alloc::vec![0u8; iov_size];
    let rc = unsafe {
        crate::hal::copy_user_chunk(iov_buf.as_mut_ptr(), iov_ptr as *const u8, iov_size)
    };
    if rc != 0 {
        return Err(Errno::Efault);
    }

    let mut total = 0usize;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_buf[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_buf[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        match sys_write_async(task, fd, base, len).await {
            Ok(n) => {
                total += n;
                if n < len {
                    break;
                }
            },
            Err(e) => {
                if total > 0 {
                    return Ok(total);
                }
                return Err(e);
            },
        }
    }
    Ok(total)
}

/// sys_ioctl_async: device control.
pub async fn sys_ioctl_async(
    task: &Arc<Task>,
    fd: u32,
    request: usize,
    argp: usize,
) -> Result<i32, Errno> {
    use crate::fs::{DeviceKind, FileObject};

    let is_console = {
        let tab = task.fd_table.lock();
        let desc = tab.get(fd).ok_or(Errno::Ebadf)?;
        matches!(
            &desc.object,
            FileObject::Device(DeviceKind::ConsoleRead | DeviceKind::ConsoleWrite)
        )
    };
    if !is_console {
        return Err(Errno::Enotty);
    }

    const TCGETS: usize = 0x5401;
    const TCSETS: usize = 0x5402;
    const TCSETSW: usize = 0x5403;
    const TCSETSF: usize = 0x5404;
    const TIOCGWINSZ: usize = 0x5413;
    const FIONBIO: usize = 0x5421;

    match request {
        TCGETS => {
            if argp != 0 {
                fault_in_user_buffer(task, argp, 60, PageFaultAccessType::WRITE).await;
                let mut termios = [0u32; 15];
                termios[0] = 0;
                termios[1] = 0;
                termios[2] = 0o000017;
                termios[3] = 0o000012;
                let rc = unsafe {
                    crate::hal::copy_user_chunk(argp as *mut u8, termios.as_ptr() as *const u8, 60)
                };
                if rc != 0 {
                    return Err(Errno::Efault);
                }
            }
            Ok(0)
        },
        TIOCGWINSZ => {
            if argp != 0 {
                fault_in_user_buffer(task, argp, 8, PageFaultAccessType::WRITE).await;
                let winsize: [u16; 4] = [24, 80, 0, 0];
                let rc = unsafe {
                    crate::hal::copy_user_chunk(argp as *mut u8, winsize.as_ptr() as *const u8, 8)
                };
                if rc != 0 {
                    return Err(Errno::Efault);
                }
            }
            Ok(0)
        },
        TCSETS | TCSETSW | TCSETSF => Ok(0),
        FIONBIO => Ok(0),
        _ => Err(Errno::Enotty),
    }
}

/// sys_ppoll_async: poll file descriptors with timeout.
pub async fn sys_ppoll_async(
    task: &Arc<Task>,
    fds_ptr: usize,
    nfds: usize,
    timeout_ptr: usize,
) -> Result<usize, Errno> {
    use crate::fs::{DeviceKind, FileObject};

    if nfds > 256 {
        return Err(Errno::Einval);
    }

    let poll_size = nfds * 8;
    if nfds > 0 {
        fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::READ).await;
    }

    let mut poll_buf = alloc::vec![0u8; poll_size];
    if nfds > 0 {
        let rc = unsafe {
            crate::hal::copy_user_chunk(poll_buf.as_mut_ptr(), fds_ptr as *const u8, poll_size)
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    let timeout_ms: Option<u64> = if timeout_ptr != 0 {
        fault_in_user_buffer(task, timeout_ptr, 16, PageFaultAccessType::READ).await;
        let mut ts_buf = [0u8; 16];
        let rc = unsafe {
            crate::hal::copy_user_chunk(ts_buf.as_mut_ptr(), timeout_ptr as *const u8, 16)
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
        let sec = i64::from_le_bytes(ts_buf[0..8].try_into().unwrap());
        let nsec = i64::from_le_bytes(ts_buf[8..16].try_into().unwrap());
        if sec < 0 || nsec < 0 {
            return Err(Errno::Einval);
        }
        Some(sec as u64 * 1000 + nsec as u64 / 1_000_000)
    } else {
        None
    };

    const POLLIN: i16 = 0x001;
    const POLLOUT: i16 = 0x004;
    const POLLERR: i16 = 0x008;
    const POLLHUP: i16 = 0x010;
    const POLLNVAL: i16 = 0x020;

    let deadline = timeout_ms.map(|ms| {
        let now = crate::hal::read_time_ms();
        now + ms
    });

    loop {
        let mut ready_count = 0usize;

        for i in 0..nfds {
            let off = i * 8;
            let fd = i32::from_le_bytes(poll_buf[off..off + 4].try_into().unwrap());
            let events = i16::from_le_bytes(poll_buf[off + 4..off + 6].try_into().unwrap());
            let mut revents: i16 = 0;

            if fd < 0 {
                poll_buf[off + 6..off + 8].copy_from_slice(&0i16.to_le_bytes());
                continue;
            }

            let tab = task.fd_table.lock();
            match tab.get(fd as u32) {
                None => {
                    revents = POLLNVAL;
                },
                Some(desc) => match &desc.object {
                    FileObject::Device(DeviceKind::ConsoleRead) => {
                        if events & POLLIN != 0 {
                            revents |= POLLIN;
                        }
                    },
                    FileObject::Device(DeviceKind::ConsoleWrite) => {
                        if events & POLLOUT != 0 {
                            revents |= POLLOUT;
                        }
                    },
                    FileObject::Device(DeviceKind::Null | DeviceKind::Zero) => {
                        revents |= events & (POLLIN | POLLOUT);
                    },
                    FileObject::PipeRead(pipe) => {
                        if pipe.readable_len() > 0 {
                            revents |= POLLIN;
                        }
                        if pipe.is_writer_closed() {
                            revents |= POLLHUP;
                        }
                    },
                    FileObject::PipeWrite(pipe) => {
                        if pipe.is_reader_closed() {
                            revents |= POLLERR;
                        } else if events & POLLOUT != 0 {
                            revents |= POLLOUT;
                        }
                    },
                    FileObject::Vnode(_) => {
                        revents |= events & (POLLIN | POLLOUT);
                    },
                },
            }

            poll_buf[off + 6..off + 8].copy_from_slice(&revents.to_le_bytes());
            if revents != 0 {
                ready_count += 1;
            }
        }

        if ready_count > 0 || matches!(timeout_ms, Some(0)) {
            if nfds > 0 {
                fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::WRITE).await;
                let rc = unsafe {
                    crate::hal::copy_user_chunk(fds_ptr as *mut u8, poll_buf.as_ptr(), poll_size)
                };
                if rc != 0 {
                    return Err(Errno::Efault);
                }
            }
            return Ok(ready_count);
        }

        if let Some(dl) = deadline {
            let now = crate::hal::read_time_ms();
            if now >= dl {
                if nfds > 0 {
                    fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::WRITE)
                        .await;
                    let rc = unsafe {
                        crate::hal::copy_user_chunk(
                            fds_ptr as *mut u8,
                            poll_buf.as_ptr(),
                            poll_size,
                        )
                    };
                    if rc != 0 {
                        return Err(Errno::Efault);
                    }
                }
                return Ok(0);
            }
        }

        if task.signals.has_actionable_pending() {
            return Err(Errno::Eintr);
        }

        crate::executor::sleep(10).await;
    }
}

/// Future for async pipe read.
pub struct PipeReadFuture<'a> {
    pipe: alloc::sync::Arc<crate::fs::Pipe>,
    task: &'a Arc<Task>,
    user_buf: usize,
    len: usize,
}

impl<'a> Future for PipeReadFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.task.signals.has_actionable_pending() {
            return Poll::Ready(Err(Errno::Eintr));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        match this.pipe.read(&mut kbuf) {
            Ok(0) => Poll::Ready(Ok(0)),
            Ok(n) => {
                let rc = unsafe {
                    crate::hal::copy_user_chunk(this.user_buf as *mut u8, kbuf.as_ptr(), n)
                };
                if rc != 0 {
                    Poll::Ready(Err(Errno::Efault))
                } else {
                    Poll::Ready(Ok(n))
                }
            },
            Err(Errno::Eagain) => {
                // Register waker FIRST, then re-check.  This closes the
                // TOCTOU race where close_write() fires between read()
                // returning Eagain and the waker being stored: close_write
                // would take None from reader_waker and never wake us.
                this.pipe.register_reader_waker(cx.waker());

                // Double-check: if close_write() raced above, the waker
                // we just stored will never be taken.  Re-try now.
                let mut kbuf2 = alloc::vec![0u8; this.len];
                match this.pipe.read(&mut kbuf2) {
                    Ok(0) => Poll::Ready(Ok(0)), // EOF — caught the race
                    Ok(n) => {
                        let rc = unsafe {
                            crate::hal::copy_user_chunk(this.user_buf as *mut u8, kbuf2.as_ptr(), n)
                        };
                        if rc != 0 {
                            Poll::Ready(Err(Errno::Efault))
                        } else {
                            Poll::Ready(Ok(n))
                        }
                    },
                    Err(Errno::Eagain) => Poll::Pending, // genuinely blocked, waker stored
                    Err(e) => Poll::Ready(Err(e)),
                }
            },
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

/// Future for async console read.
pub struct ConsoleReadFuture<'a> {
    task: &'a Arc<Task>,
    user_buf: usize,
    len: usize,
}

impl<'a> Future for ConsoleReadFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.task.signals.has_actionable_pending() {
            return Poll::Ready(Err(Errno::Eintr));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        let n = crate::console::console_read(&mut kbuf);
        if n > 0 {
            let rc =
                unsafe { crate::hal::copy_user_chunk(this.user_buf as *mut u8, kbuf.as_ptr(), n) };
            if rc != 0 {
                Poll::Ready(Err(Errno::Efault))
            } else {
                Poll::Ready(Ok(n))
            }
        } else {
            crate::console::console_register_waker(cx.waker());
            Poll::Pending
        }
    }
}

/// Future for async pipe write.
pub struct PipeWriteFuture<'a> {
    pipe: alloc::sync::Arc<crate::fs::Pipe>,
    task: &'a Arc<Task>,
    data: alloc::vec::Vec<u8>,
    written: usize,
}

impl<'a> Future for PipeWriteFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.task.signals.has_actionable_pending() {
            if this.written > 0 {
                return Poll::Ready(Ok(this.written));
            }
            return Poll::Ready(Err(Errno::Eintr));
        }
        if this.written >= this.data.len() {
            return Poll::Ready(Ok(this.written));
        }
        match this.pipe.write(&this.data[this.written..]) {
            Ok(n) => {
                this.written += n;
                if this.written >= this.data.len() {
                    return Poll::Ready(Ok(this.written));
                }
                // Partial write — register waker, then double-check for
                // close_read() race (same TOCTOU pattern as PipeReadFuture).
                this.pipe.register_writer_waker(cx.waker());
                match this.pipe.write(&this.data[this.written..]) {
                    Ok(n2) => {
                        this.written += n2;
                        if this.written >= this.data.len() {
                            Poll::Ready(Ok(this.written))
                        } else {
                            Poll::Pending
                        }
                    },
                    Err(Errno::Eagain) => Poll::Pending, // genuinely full, waker stored
                    Err(Errno::Epipe) => {
                        this.task.signals.post_signal(crate::proc::SIGPIPE);
                        Poll::Ready(Err(Errno::Epipe))
                    },
                    Err(e) => Poll::Ready(Err(e)),
                }
            },
            Err(Errno::Eagain) => {
                // Register waker FIRST, then re-check.  Closes the TOCTOU
                // race where close_read() fires between write() returning
                // Eagain and the waker being stored.
                this.pipe.register_writer_waker(cx.waker());
                match this.pipe.write(&this.data[this.written..]) {
                    Ok(n) => {
                        this.written += n;
                        if this.written >= this.data.len() {
                            Poll::Ready(Ok(this.written))
                        } else {
                            Poll::Pending
                        }
                    },
                    Err(Errno::Eagain) => Poll::Pending, // genuinely full, waker stored
                    Err(Errno::Epipe) => {
                        this.task.signals.post_signal(crate::proc::SIGPIPE);
                        Poll::Ready(Err(Errno::Epipe))
                    },
                    Err(e) => Poll::Ready(Err(e)),
                }
            },
            Err(Errno::Epipe) => {
                // Post SIGPIPE when writing to a pipe with no readers
                this.task.signals.post_signal(crate::proc::SIGPIPE);
                Poll::Ready(Err(Errno::Epipe))
            },
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

// ── 阶段 A：目录功能系统调用 ────────────────────────────────────────

/// Linux `struct linux_dirent64` layout for getdents64.
#[repr(C)]
struct LinuxDirent64 {
    d_ino: u64,
    d_off: i64,
    d_reclen: u16,
    d_type: u8,
    // d_name follows (variable length, null-terminated)
}

/// sys_mkdirat: create a directory relative to dirfd.
pub async fn sys_mkdirat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    _mode: usize,
) -> Result<(), Errno> {
    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let path_str = absolutize_path(task, dirfd, &raw_path)?;

    // Check if already exists
    let exists = crate::fs::fs_lookup(0, &path_str).await;
    if exists.is_ok() {
        return Err(Errno::Eexist);
    }

    crate::fs::fs_mkdir(&path_str)
        .await
        .map_err(|_| Errno::Eio)?;

    crate::klog!(syscall, debug, "mkdirat: created {}", path_str);
    Ok(())
}

/// sys_unlinkat: remove a file or directory.
pub async fn sys_unlinkat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    flags: i32,
) -> Result<(), Errno> {
    const AT_REMOVEDIR: i32 = 0x200;

    let raw_path = copyinstr(task, pathname_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    if raw_path == "." {
        return Err(Errno::Einval);
    }
    let path_str = absolutize_path(task, dirfd, &raw_path)?;

    // Check existence and type
    let (child_ino, ftype, _) = crate::fs::fs_lookup(0, &path_str)
        .await
        .map_err(|_| Errno::Enoent)?;
    let is_dir = ftype == 2;

    if (flags & AT_REMOVEDIR) != 0 && !is_dir {
        return Err(Errno::Enotdir);
    }
    if (flags & AT_REMOVEDIR) == 0 && is_dir {
        return Err(Errno::Eisdir);
    }

    crate::fs::fs_unlink(&path_str, is_dir)
        .await
        .map_err(|_| Errno::Eio)?;

    // Invalidate dentry cache for this entry
    if let Some(last_slash) = path_str.rfind('/') {
        let parent_path = if last_slash == 0 {
            "/"
        } else {
            &path_str[..last_slash]
        };
        let child_name = &path_str[last_slash + 1..];
        // Resolve parent vnode id for dentry invalidation
        if let Ok(vnode) = crate::fs::resolve(parent_path).await {
            crate::fs::invalidate_dentry(vnode.vnode_id(), child_name);
        }
    }
    if !is_dir {
        vnode_destroy_object(child_ino as u64);
    }

    crate::klog!(syscall, debug, "unlinkat: removed {}", path_str);
    Ok(())
}

/// sys_symlinkat: create a symbolic link on ext4.
pub async fn sys_symlinkat_async(
    task: &Arc<Task>,
    target_ptr: usize,
    newdirfd: isize,
    linkpath_ptr: usize,
) -> Result<(), Errno> {
    if target_ptr == 0 || linkpath_ptr == 0 {
        return Err(Errno::Efault);
    }

    let raw_target = copyinstr(task, target_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    let raw_linkpath = copyinstr(task, linkpath_ptr, 256)
        .await
        .ok_or(Errno::Efault)?;
    if raw_target.is_empty() || raw_linkpath.is_empty() {
        return Err(Errno::Einval);
    }

    let link_abs = absolutize_path(task, newdirfd, &raw_linkpath)?;
    let parent = if let Some(pos) = link_abs.rfind('/') {
        if pos == 0 {
            "/"
        } else {
            &link_abs[..pos]
        }
    } else {
        "/"
    };

    let parent_vnode = crate::fs::resolve(parent)
        .await
        .map_err(|_| Errno::Enoent)?;
    if parent_vnode.vtype() != crate::fs::VnodeType::Directory {
        return Err(Errno::Enotdir);
    }

    let target_abs = if raw_target.starts_with('/') {
        normalize_absolute_path(&raw_target)
    } else {
        let mut joined = String::from(parent);
        if joined != "/" {
            joined.push('/');
        }
        joined.push_str(&raw_target);
        normalize_absolute_path(&joined)
    };

    fs::fs_symlink(&target_abs, &link_abs).await?;
    crate::klog!(syscall, debug, "symlinkat: {} -> {}", link_abs, target_abs);
    Ok(())
}

/// sys_getdents64: read directory entries into user buffer.
pub async fn sys_getdents64_async(
    task: &Arc<Task>,
    fd: u32,
    buf_ptr: usize,
    buf_len: usize,
) -> Result<usize, Errno> {
    const LEN_BEFORE_NAME: usize = 19; // sizeof(d_ino) + sizeof(d_off) + sizeof(d_reclen) + sizeof(d_type)

    if buf_ptr == 0 || buf_len == 0 {
        return Err(Errno::Einval);
    }

    // Get the directory path from fd
    let (dir_path, desc) = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::Ebadf)?;
        match &d.object {
            FileObject::Vnode(v) => {
                if v.vtype() != VnodeType::Directory {
                    return Err(Errno::Enotdir);
                }
                (String::from(v.path()), Arc::clone(d))
            },
            _ => return Err(Errno::Enotdir),
        }
    };

    let dir_path = if dir_path.is_empty() {
        String::from("/")
    } else {
        dir_path
    };

    // Get current offset (used as entry index)
    let start_idx = desc.offset.load(Ordering::Relaxed) as usize;

    // Read a page of directory entries from current logical offset.
    let (entries, count) = crate::fs::fs_readdir(&dir_path, start_idx)
        .await
        .map_err(|_| Errno::Eio)?;
    if count == 0 {
        return Ok(0);
    }

    // Fault in user buffer
    fault_in_user_buffer(task, buf_ptr, buf_len, PageFaultAccessType::WRITE).await;

    let mut written: usize = 0;
    let mut idx = 0usize;

    while idx < count {
        let entry = &entries[idx];
        let name_len = entry.name_len as usize;
        let name_bytes = &entry.name[..name_len];

        // c_name_len includes null terminator
        let c_name_len = name_len + 1;
        let rec_len = (LEN_BEFORE_NAME + c_name_len + 7) & !0x7; // align to 8

        if written + rec_len > buf_len {
            break;
        }

        // Map lwext4 inode_type to Linux d_type
        let d_type: u8 = match entry.inode_type {
            2 => 4,  // EXT4_DE_DIR -> DT_DIR
            1 => 8,  // EXT4_DE_REG_FILE -> DT_REG
            7 => 10, // EXT4_DE_SYMLINK -> DT_LNK
            _ => 0,  // DT_UNKNOWN
        };

        let dirent = LinuxDirent64 {
            d_ino: entry.inode as u64,
            d_off: (start_idx + idx + 1) as i64,
            d_reclen: rec_len as u16,
            d_type,
        };

        // Write dirent header to user memory
        let dst = (buf_ptr + written) as *mut u8;
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                dst,
                &dirent as *const LinuxDirent64 as *const u8,
                LEN_BEFORE_NAME,
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }

        // Write name + null terminator
        let name_dst = (buf_ptr + written + LEN_BEFORE_NAME) as *mut u8;
        let rc = unsafe { crate::hal::copy_user_chunk(name_dst, name_bytes.as_ptr(), name_len) };
        if rc != 0 {
            return Err(Errno::Efault);
        }
        // Null terminator + padding zeros
        let pad_start = buf_ptr + written + LEN_BEFORE_NAME + name_len;
        let pad_len = rec_len - LEN_BEFORE_NAME - name_len;
        let zeros = [0u8; 8];
        let rc =
            unsafe { crate::hal::copy_user_chunk(pad_start as *mut u8, zeros.as_ptr(), pad_len) };
        if rc != 0 {
            return Err(Errno::Efault);
        }

        written += rec_len;
        idx += 1;
    }

    // Update offset to track position
    desc.offset
        .store((start_idx + idx) as u64, Ordering::Relaxed);

    Ok(written)
}
