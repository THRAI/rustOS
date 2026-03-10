#![allow(dead_code)]
//! Filesystem delegate: serializes all ext4 operations off the async executor.
//!
//! A single async task owns the lwext4 mount and processes requests
//! from a bounded channel. Callers use async functions (fs_open, fs_read, etc.)
//! that send requests and await replies via oneshot channels.

use crate::fs::{
    ext4_cache_flush, ext4_close, ext4_dir_close, ext4_dir_next, ext4_dir_open, ext4_link,
    ext4_mkdir, ext4_mount, ext4_open, ext4_read, ext4_readlink, ext4_rename, ext4_stat,
    ext4_symlink, ext4_truncate, ext4_unlink, ext4_write, DelegateToken,
};
use crate::hal_common::IrqSafeSpinLock;
use crate::hal_common::PhysAddr;
use alloc::collections::VecDeque;
use alloc::string::String;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Poll, Waker};
use core::future::Future;
use crate::hal_common::IrqSafeSpinLock;
use crate::hal_common::Errno;
use lwext4_rust::Ext4File;
// SAFETY: Ext4File contains raw pointers from lwext4 C code.
// All access is serialized in the single delegate_task — never shared across threads.
struct SendExt4File(Ext4File);
unsafe impl Send for SendExt4File {}

#[inline]
fn map_backend_path(path: &str) -> String {
    crate::fs::resolve_to_source(path)
}

/// Maximum pending requests in the channel.
const CHANNEL_CAPACITY: usize = 256;

/// File handle (index into delegate's open file table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsFileHandle(pub u16);

/// A directory entry returned by ReadDir.
#[derive(Clone, Copy)]
pub struct DirEntryRaw {
    pub name: [u8; 255],
    pub name_len: u8,
    pub inode_type: u8,
    pub inode: u32,
}

/// ReadDir result: up to 32 entries per batch.
pub const READDIR_BATCH: usize = 32;

/// Filesystem request sent to the delegate.
pub enum FsRequest {
    Open {
        path: [u8; 256],
        path_len: usize,
        flags: u32,
        reply: ReplySlot<Result<FsFileHandle, Errno>>,
    },
    Read {
        handle: FsFileHandle,
        buf_ptr: usize,
        len: usize,
        reply: ReplySlot<Result<usize, Errno>>,
    },
    Write {
        handle: FsFileHandle,
        buf_ptr: usize,
        len: usize,
        reply: ReplySlot<Result<usize, Errno>>,
    },
    Close {
        handle: FsFileHandle,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Look up a child entry in a directory by parent path + name.
    /// Returns (child_ino, file_type, file_size).
    Lookup {
        parent_ino: u32,
        name: [u8; 256],
        name_len: usize,
        reply: ReplySlot<Result<(u32, u8, u64), Errno>>,
    },
    /// Stat an inode: returns (size, file_type_u8).
    Stat {
        ino: u32,
        reply: ReplySlot<Result<(u64, u8), Errno>>,
    },
    /// Read one page of file data into the given pa physical address, at the given byte offset.
    ReadPage {
        path: [u8; 256],
        path_len: usize,
        offset: u64,
        pa: usize,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Write data to a file at the given byte offset.
    /// Opens file, seeks, writes, closes. Returns bytes written.
    WriteAt {
        path: [u8; 256],
        path_len: usize,
        offset: u64,
        data_ptr: usize,
        data_len: usize,
        reply: ReplySlot<Result<usize, Errno>>,
    },
    /// Truncate file at path to the given size.
    Truncate {
        path: [u8; 256],
        path_len: usize,
        size: u64,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Create a directory at the given path.
    Mkdir {
        path: [u8; 256],
        path_len: usize,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Remove a file or directory at the given path.
    /// `is_dir`: true for rmdir, false for unlink.
    Unlink {
        path: [u8; 256],
        path_len: usize,
        is_dir: bool,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Create hard link: `new_path` links to `old_path`.
    Link {
        old_path: [u8; 256],
        old_len: usize,
        new_path: [u8; 256],
        new_len: usize,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Rename/move path.
    Rename {
        old_path: [u8; 256],
        old_len: usize,
        new_path: [u8; 256],
        new_len: usize,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Create symlink: `path` -> `target`.
    Symlink {
        target: [u8; 256],
        target_len: usize,
        path: [u8; 256],
        path_len: usize,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Read symlink target by path.
    ReadLink {
        path: [u8; 256],
        path_len: usize,
        reply: ReplySlot<Result<(usize, [u8; 256]), Errno>>,
    },
    /// Flush cache by path (mount point dispatch is done in lwext4).
    CacheFlush {
        path: [u8; 256],
        path_len: usize,
        reply: ReplySlot<Result<(), Errno>>,
    },
    /// Read all directory entries from a directory path.
    /// Returns (entries, count) starting from `start_idx`.
    ReadDir {
        path: [u8; 256],
        path_len: usize,
        start_idx: usize,
        reply: ReplySlot<Result<([DirEntryRaw; READDIR_BATCH], usize), Errno>>,
    },
}

/// A oneshot reply slot: delegate writes result, wakes the caller.
pub struct ReplySlot<T: 'static> {
    inner: &'static ReplyInner<T>,
}

struct ReplyInner<T> {
    done: AtomicBool,
    waker: IrqSafeSpinLock<Option<Waker>>,
    value: IrqSafeSpinLock<Option<T>>,
}

impl<T: 'static> ReplySlot<T> {
    fn new(inner: &'static ReplyInner<T>) -> Self {
        Self { inner }
    }

    fn complete(&self, val: T) {
        *self.inner.value.lock() = Some(val);
        self.inner.done.store(true, Ordering::Release);
        if let Some(w) = self.inner.waker.lock().take() {
            w.wake();
        }
    }
}

/// Future that waits for a reply.
struct ReplyFuture<T: Copy + 'static> {
    inner: &'static ReplyInner<T>,
}

impl<T: Copy + 'static> Future for ReplyFuture<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> Poll<T> {
        if self.inner.done.load(Ordering::Acquire) {
            let val = self.inner.value.lock().take().unwrap();
            return Poll::Ready(val);
        }
        *self.inner.waker.lock() = Some(cx.waker().clone());
        // Double-check after registering waker
        if self.inner.done.load(Ordering::Acquire) {
            let val = self.inner.value.lock().take().unwrap();
            return Poll::Ready(val);
        }
        Poll::Pending
    }
}

/// Allocate a static reply slot. Uses a pool of pre-allocated slots.
const REPLY_POOL_SIZE: usize = 64;

macro_rules! define_reply_pool {
    ($name:ident, $T:ty) => {
        static $name: [ReplyInner<$T>; REPLY_POOL_SIZE] = {
            #[allow(clippy::declare_interior_mutable_const)]
            const INIT: ReplyInner<$T> = ReplyInner {
                done: AtomicBool::new(false),
                waker: IrqSafeSpinLock::new(None),
                value: IrqSafeSpinLock::new(None),
            };
            [INIT; REPLY_POOL_SIZE]
        };
    };
}

define_reply_pool!(OPEN_REPLIES, Result<FsFileHandle, Errno>);
define_reply_pool!(READ_REPLIES, Result<usize, Errno>);
define_reply_pool!(WRITE_REPLIES, Result<usize, Errno>);
define_reply_pool!(CLOSE_REPLIES, Result<(), Errno>);
define_reply_pool!(LOOKUP_REPLIES, Result<(u32, u8, u64), Errno>);
define_reply_pool!(STAT_REPLIES, Result<(u64, u8), Errno>);
define_reply_pool!(READPAGE_REPLIES, Result<(), Errno>);
define_reply_pool!(WRITEAT_REPLIES, Result<usize, Errno>);
define_reply_pool!(TRUNCATE_REPLIES, Result<(), Errno>);
define_reply_pool!(MKDIR_REPLIES, Result<(), Errno>);
define_reply_pool!(UNLINK_REPLIES, Result<(), Errno>);
define_reply_pool!(LINK_REPLIES, Result<(), Errno>);
define_reply_pool!(RENAME_REPLIES, Result<(), Errno>);
define_reply_pool!(SYMLINK_REPLIES, Result<(), Errno>);
define_reply_pool!(READLINK_REPLIES, Result<(usize, [u8; 256]), Errno>);
define_reply_pool!(CACHEFLUSH_REPLIES, Result<(), Errno>);
define_reply_pool!(READDIR_REPLIES, Result<([DirEntryRaw; READDIR_BATCH], usize), Errno>);

static OPEN_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READ_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static WRITE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static CLOSE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static LOOKUP_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static STAT_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READPAGE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static WRITEAT_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static TRUNCATE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static MKDIR_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static UNLINK_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static LINK_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static RENAME_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static SYMLINK_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READLINK_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static CACHEFLUSH_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READDIR_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);

macro_rules! define_alloc_reply {
    ($fn_name:ident, $pool:ident, $idx:ident, $T:ty) => {
        fn $fn_name() -> &'static ReplyInner<$T> {
            let idx = $idx.fetch_add(1, Ordering::Relaxed) % REPLY_POOL_SIZE;
            let r = &$pool[idx];
            r.done.store(false, Ordering::Relaxed);
            *r.waker.lock() = None;
            *r.value.lock() = None;
            r
        }
    };
}

define_alloc_reply!(alloc_open_reply,       OPEN_REPLIES,       OPEN_REPLY_IDX,   Result<FsFileHandle, Errno>);
define_alloc_reply!(alloc_read_reply,       READ_REPLIES,       READ_REPLY_IDX,   Result<usize, Errno>);
define_alloc_reply!(alloc_write_reply,      WRITE_REPLIES,      WRITE_REPLY_IDX,  Result<usize, Errno>);
define_alloc_reply!(alloc_close_reply,      CLOSE_REPLIES,      CLOSE_REPLY_IDX,  Result<(), Errno>);
define_alloc_reply!(alloc_lookup_reply,     LOOKUP_REPLIES,     LOOKUP_REPLY_IDX, Result<(u32, u8, u64), Errno>);
define_alloc_reply!(alloc_stat_reply,       STAT_REPLIES,       STAT_REPLY_IDX,   Result<(u64, u8), Errno>);
define_alloc_reply!(alloc_readpage_reply,   READPAGE_REPLIES,   READPAGE_REPLY_IDX, Result<(), Errno>);
define_alloc_reply!(alloc_writeat_reply,    WRITEAT_REPLIES,    WRITEAT_REPLY_IDX, Result<usize, Errno>);
define_alloc_reply!(alloc_truncate_reply,   TRUNCATE_REPLIES,   TRUNCATE_REPLY_IDX, Result<(), Errno>);
define_alloc_reply!(alloc_mkdir_reply,      MKDIR_REPLIES,      MKDIR_REPLY_IDX, Result<(), Errno>);
define_alloc_reply!(alloc_unlink_reply,     UNLINK_REPLIES,     UNLINK_REPLY_IDX, Result<(), Errno>);
define_alloc_reply!(alloc_link_reply,       LINK_REPLIES,       LINK_REPLY_IDX, Result<(), Errno>);
define_alloc_reply!(alloc_rename_reply,     RENAME_REPLIES,     RENAME_REPLY_IDX, Result<(), Errno>);
define_alloc_reply!(alloc_symlink_reply,    SYMLINK_REPLIES,    SYMLINK_REPLY_IDX, Result<(), Errno>);
define_alloc_reply!( alloc_readlink_reply,  READLINK_REPLIES,   READLINK_REPLY_IDX, Result<(usize, [u8; 256]), Errno> );
define_alloc_reply!( alloc_cacheflush_reply, CACHEFLUSH_REPLIES, CACHEFLUSH_REPLY_IDX, Result<(), Errno> );
define_alloc_reply!(alloc_readdir_reply,    READDIR_REPLIES,    READDIR_REPLY_IDX, Result<([DirEntryRaw; READDIR_BATCH], usize), Errno>);

/// Bounded request channel.
static REQUEST_QUEUE: IrqSafeSpinLock<VecDeque<FsRequest>> = IrqSafeSpinLock::new(VecDeque::new());
static REQUEST_COUNT: AtomicUsize = AtomicUsize::new(0);
static DELEGATE_WAKER: IrqSafeSpinLock<Option<Waker>> = IrqSafeSpinLock::new(None);

/// Send a request to the delegate channel.
fn send_request(req: FsRequest) {
    {
        let mut q = REQUEST_QUEUE.lock();
        q.push_back(req);
    }
    REQUEST_COUNT.fetch_add(1, Ordering::Release);
    if let Some(w) = DELEGATE_WAKER.lock().take() {
        w.wake();
    }
}

/// Maximum open files.
const MAX_OPEN_FILES: usize = 64;

/// Delegate task: the single async task that owns lwext4 and processes requests.
async fn delegate_task() {
    // Mount the ext4 filesystem via lwext4
    if let Err(e) = crate::fs::ext4::mount() {
        klog!(fs, error, "ext4 mount failed: {:?}", e);
        return;
    }
    klog!(fs, info, "ext4 mounted, delegate running");

    // SAFETY: We are the single delegate task — the only holder of this token.
    let mut tok = unsafe { DelegateToken::new() };

    // Open file table: each slot holds an Option<SendExt4File>
    let mut open_files: [Option<SendExt4File>; MAX_OPEN_FILES] = [const { None }; MAX_OPEN_FILES];

    // Process requests forever
    loop {
        crate::klog!(
            fs,
            debug,
            "delegate_task: awaiting request. queue={}",
            REQUEST_COUNT.load(Ordering::Acquire)
        );
        let req = DelegateRecvFuture.await;

        crate::klog!(fs, debug, "delegate_task: received request (enum variant)");
        match req {
            FsRequest::Open {
                path,
                path_len,
                flags,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                match ext4_open(&mut tok, &backend_path, flags) {
                    Ok(file) => {
                        let mut slot = None;
                        for (i, f) in open_files.iter().enumerate() {
                            if f.is_none() {
                                slot = Some(i);
                                break;
                            }
                        }
                        match slot {
                            Some(i) => {
                                open_files[i] = Some(SendExt4File(file));
                                reply.complete(Ok(FsFileHandle(i as u16)));
                            }
                            None => reply.complete(Err(Errno::Emfile)), // EMFILE
                        }
                    }
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Read {
                handle,
                buf_ptr,
                len,
                reply,
            } => {
                let idx = handle.0 as usize;
                if idx >= MAX_OPEN_FILES || open_files[idx].is_none() {
                    reply.complete(Err(Errno::Ebadf)); // EBADF
                    crate::executor::yield_now().await;
                    continue;
                }
                let file = &mut open_files[idx].as_mut().unwrap().0;
                let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len) };
                match ext4_read(&mut tok, file, buf) {
                    Ok(n) => reply.complete(Ok(n)),
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Write {
                handle,
                buf_ptr,
                len,
                reply,
            } => {
                let idx = handle.0 as usize;
                if idx >= MAX_OPEN_FILES || open_files[idx].is_none() {
                    reply.complete(Err(Errno::Ebadf)); // EBADF
                    crate::executor::yield_now().await;
                    continue;
                }
                let file = &mut open_files[idx].as_mut().unwrap().0;
                let buf = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, len) };
                match ext4_write(&mut tok, file, buf) {
                    Ok(n) => reply.complete(Ok(n)),
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Close { handle, reply } => {
                let idx = handle.0 as usize;
                if idx < MAX_OPEN_FILES {
                    if let Some(ref mut wrapper) = open_files[idx] {
                        let _ = ext4_close(&mut tok, &mut wrapper.0);
                    }
                    open_files[idx] = None;
                }
                reply.complete(Ok(()));
            }
            FsRequest::Lookup {
                parent_ino: _,
                name,
                name_len,
                reply,
            } => {
                // name is already a full path (e.g. "/bin/init")
                let full_path = core::str::from_utf8(&name[..name_len]).unwrap_or("");
                let backend_path = map_backend_path(full_path);
                klog!(fs, debug, "lookup: {:?} -> {:?}", full_path, backend_path);
                match ext4_stat(&mut tok, &backend_path) {
                    Ok((ino, size, ftype)) => {
                        reply.complete(Ok((ino, ftype, size)));
                    }
                    Err(_) => reply.complete(Err(Errno::Enoent)), // ENOENT
                }
            }
            FsRequest::Stat { ino: _, reply } => {
                // Stat not directly usable without a path in lwext4.
                // Callers should use Lookup which returns size+type.
                reply.complete(Err(Errno::Enosys)); // ENOSYS
            }
            FsRequest::ReadPage {
                path,
                path_len,
                offset,
                pa,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                // Fill the caller-provided page with file contents.
                let buf = unsafe { core::slice::from_raw_parts_mut(pa as *mut u8, 4096) };
                buf.fill(0);
                match ext4_open(&mut tok, &backend_path, 0) {
                    Ok(mut file) => {
                        let _ = file.file_seek(offset as i64, 0); // SEEK_SET
                        let _ = ext4_read(&mut tok, &mut file, buf);
                        let _ = ext4_close(&mut tok, &mut file);
                        reply.complete(Ok(()));
                    }
                    Err(_) => reply.complete(Err(Errno::Enomem)), //Enomem
                }
            }
            FsRequest::WriteAt {
                path,
                path_len,
                offset,
                data_ptr,
                data_len,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                let data = unsafe { core::slice::from_raw_parts(data_ptr as *const u8, data_len) };
                // Open with write flags, seek, write, close
                match ext4_open(&mut tok, &backend_path, 0x0002) {
                    Ok(mut file) => {
                        let _ = file.file_seek(offset as i64, 0); // SEEK_SET
                        match ext4_write(&mut tok, &mut file, data) {
                            Ok(n) => {
                                let _ = ext4_close(&mut tok, &mut file);
                                reply.complete(Ok(n));
                            }
                            Err(e) => {
                                let _ = ext4_close(&mut tok, &mut file);
                                reply.complete(Err(e));
                            }
                        }
                    }
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Truncate {
                path,
                path_len,
                size,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                reply.complete(ext4_truncate(&mut tok, &backend_path, size));
            }
            FsRequest::Mkdir {
                path,
                path_len,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                reply.complete(crate::fs::ext4::mkdir(&mut tok, &backend_path));
            }
            FsRequest::Unlink {
                path,
                path_len,
                is_dir,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                let result = if is_dir {
                    // lwext4 dir_rm: use file_remove which handles dirs too
                    ext4_unlink(&mut tok, &backend_path)
                } else {
                    ext4_unlink(&mut tok, &backend_path)
                };
                reply.complete(result);
            }
            FsRequest::Link {
                old_path,
                old_len,
                new_path,
                new_len,
                reply,
            } => {
                let old_path_str = core::str::from_utf8(&old_path[..old_len]).unwrap_or("");
                let new_path_str = core::str::from_utf8(&new_path[..new_len]).unwrap_or("");
                let old_backend = map_backend_path(old_path_str);
                let new_backend = map_backend_path(new_path_str);
                reply.complete(
                    crate::fs::ext4::link(&mut tok, &old_backend, &new_backend),
                );
            }
            FsRequest::Rename {
                old_path,
                old_len,
                new_path,
                new_len,
                reply,
            } => {
                let old_path_str = core::str::from_utf8(&old_path[..old_len]).unwrap_or("");
                let new_path_str = core::str::from_utf8(&new_path[..new_len]).unwrap_or("");
                let old_backend = map_backend_path(old_path_str);
                let new_backend = map_backend_path(new_path_str);
                reply.complete(
                    crate::fs::ext4::rename(&mut tok, &old_backend, &new_backend),
                );
            }
            FsRequest::Symlink {
                target,
                target_len,
                path,
                path_len,
                reply,
            } => {
                let target_str = core::str::from_utf8(&target[..target_len]).unwrap_or("");
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                reply.complete(
                    crate::fs::ext4::symlink(&mut tok, target_str, &backend_path),
                );
            }
            FsRequest::ReadLink {
                path,
                path_len,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                let mut out = [0u8; 256];
                match ext4_readlink(&mut tok, &backend_path, &mut out) {
                    Ok(n) => reply.complete(Ok((n, out))),
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::CacheFlush {
                path,
                path_len,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                reply.complete(crate::fs::ext4::cache_flush(&mut tok, &backend_path));
            }
            FsRequest::ReadDir {
                path,
                path_len,
                start_idx,
                reply,
            } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let backend_path = map_backend_path(path_str);
                match ext4_dir_open(&mut tok, &backend_path) {
                    Ok(mut dir) => {
                        let mut skipped = 0usize;
                        while skipped < start_idx {
                            if ext4_dir_next(&mut tok, &mut dir).is_none() {
                                break;
                            }
                            skipped += 1;
                        }
                        let mut entries = [DirEntryRaw {
                            name: [0u8; 255],
                            name_len: 0,
                            inode_type: 0,
                            inode: 0,
                        }; READDIR_BATCH];
                        let mut count = 0usize;
                        while count < READDIR_BATCH {
                            match ext4_dir_next(&mut tok, &mut dir) {
                                Some((name, name_len, itype, ino)) => {
                                    entries[count] = DirEntryRaw {
                                        name,
                                        name_len,
                                        inode_type: itype,
                                        inode: ino,
                                    };
                                    count += 1;
                                }
                                None => break,
                            }
                        }
                        let _ = ext4_dir_close(&mut tok, &mut dir);
                        reply.complete(Ok((entries, count)));
                    }
                    Err(e) => reply.complete(Err(e)),
                }
            }
        }

        // Yield to let other tasks run
        crate::executor::yield_now().await;
    }
}

/// Future that receives the next request from the channel.
struct DelegateRecvFuture;

impl Future for DelegateRecvFuture {
    type Output = FsRequest;

    fn poll(self: Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> Poll<FsRequest> {
        if REQUEST_COUNT.load(Ordering::Acquire) > 0 {
            let mut q = REQUEST_QUEUE.lock();
            if let Some(req) = q.pop_front() {
                REQUEST_COUNT.fetch_sub(1, Ordering::Release);
                return Poll::Ready(req);
            }
        }
        *DELEGATE_WAKER.lock() = Some(cx.waker().clone());
        // Double-check
        if REQUEST_COUNT.load(Ordering::Acquire) > 0 {
            let mut q = REQUEST_QUEUE.lock();
            if let Some(req) = q.pop_front() {
                REQUEST_COUNT.fetch_sub(1, Ordering::Release);
                return Poll::Ready(req);
            }
        }
        Poll::Pending
    }
}

// --- Public async API ---

/// Open a file by path. Returns a file handle.
pub async fn fs_open(path: &str) -> Result<FsFileHandle, Errno> {
    fs_open_flags(path, 0).await
}

/// Open a file by path with flags. Returns a file handle.
pub async fn fs_open_flags(path: &str, flags: u32) -> Result<FsFileHandle, Errno> {
    let reply_inner = alloc_open_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::Open {
        path: path_buf,
        path_len: len,
        flags,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Read from an open file into buf. Returns bytes read.
pub async fn fs_read(handle: FsFileHandle, buf: &mut [u8]) -> Result<usize, Errno> {
    let reply_inner = alloc_read_reply();

    send_request(FsRequest::Read {
        handle,
        buf_ptr: buf.as_mut_ptr() as usize,
        len: buf.len(),
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Write to an open file from buf. Returns bytes written.
pub async fn fs_write(handle: FsFileHandle, buf: &[u8]) -> Result<usize, Errno> {
    let reply_inner = alloc_write_reply();

    send_request(FsRequest::Write {
        handle,
        buf_ptr: buf.as_ptr() as usize,
        len: buf.len(),
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Close an open file handle.
pub async fn fs_close(handle: FsFileHandle) -> Result<(), Errno> {
    let reply_inner = alloc_close_reply();

    send_request(FsRequest::Close {
        handle,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Look up a child entry in a directory. Returns (child_ino, file_type, file_size).
pub async fn fs_lookup(parent_ino: u32, name: &str) -> Result<(u32, u8, u64), Errno> {
    let reply_inner = alloc_lookup_reply();
    let mut name_buf = [0u8; 256];
    let len = name.len().min(256);
    name_buf[..len].copy_from_slice(&name.as_bytes()[..len]);

    send_request(FsRequest::Lookup {
        parent_ino,
        name: name_buf,
        name_len: len,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Stat an inode. Returns (size, file_type_u8).
pub async fn fs_stat(ino: u32) -> Result<(u64, u8), Errno> {
    let reply_inner = alloc_stat_reply();

    send_request(FsRequest::Stat {
        ino,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Read one page of file data into the given pa physical address, at the given byte offset.
pub async fn fs_read_page(path: &str, offset: u64, pa: PhysAddr) -> Result<(), Errno> {
    crate::klog!(
        fs,
        debug,
        "fs_read_page: path={}, offset={}, pa={:#x}",
        path,
        offset,
        pa.0
    );
    let reply_inner = alloc_readpage_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::ReadPage {
        path: path_buf,
        path_len: len,
        offset,
        pa: pa.0,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Write data to a file at the given byte offset.
/// Opens file, seeks, writes, closes. Returns bytes written.
pub async fn fs_write_at(path: &str, offset: u64, data: &[u8]) -> Result<usize, Errno> {
    let reply_inner = alloc_writeat_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::WriteAt {
        path: path_buf,
        path_len: len,
        offset,
        data_ptr: data.as_ptr() as usize,
        data_len: data.len(),
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Truncate file at path to `size`.
pub async fn fs_truncate(path: &str, size: u64) -> Result<(), Errno> {
    let reply_inner = alloc_truncate_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::Truncate {
        path: path_buf,
        path_len: len,
        size,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Create a directory at the given path.
pub async fn fs_mkdir(path: &str) -> Result<(), Errno> {
    let reply_inner = alloc_mkdir_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::Mkdir {
        path: path_buf,
        path_len: len,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Remove a file or directory at the given path.
pub async fn fs_unlink(path: &str, is_dir: bool) -> Result<(), Errno> {
    let reply_inner = alloc_unlink_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::Unlink {
        path: path_buf,
        path_len: len,
        is_dir,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Create hard link `new_path` -> `old_path`.
pub async fn fs_link(old_path: &str, new_path: &str) -> Result<(), Errno> {
    let reply_inner = alloc_link_reply();
    let mut old_buf = [0u8; 256];
    let mut new_buf = [0u8; 256];
    let old_len = old_path.len().min(256);
    let new_len = new_path.len().min(256);
    old_buf[..old_len].copy_from_slice(&old_path.as_bytes()[..old_len]);
    new_buf[..new_len].copy_from_slice(&new_path.as_bytes()[..new_len]);
    send_request(FsRequest::Link {
        old_path: old_buf,
        old_len,
        new_path: new_buf,
        new_len,
        reply: ReplySlot::new(reply_inner),
    });
    ReplyFuture { inner: reply_inner }.await
}

/// Rename/move path.
pub async fn fs_rename(old_path: &str, new_path: &str) -> Result<(), Errno> {
    let reply_inner = alloc_rename_reply();
    let mut old_buf = [0u8; 256];
    let mut new_buf = [0u8; 256];
    let old_len = old_path.len().min(256);
    let new_len = new_path.len().min(256);
    old_buf[..old_len].copy_from_slice(&old_path.as_bytes()[..old_len]);
    new_buf[..new_len].copy_from_slice(&new_path.as_bytes()[..new_len]);
    send_request(FsRequest::Rename {
        old_path: old_buf,
        old_len,
        new_path: new_buf,
        new_len,
        reply: ReplySlot::new(reply_inner),
    });
    ReplyFuture { inner: reply_inner }.await
}

/// Create symbolic link `path` -> `target`.
pub async fn fs_symlink(target: &str, path: &str) -> Result<(), Errno> {
    let reply_inner = alloc_symlink_reply();
    let mut target_buf = [0u8; 256];
    let mut path_buf = [0u8; 256];
    let target_len = target.len().min(256);
    let path_len = path.len().min(256);
    target_buf[..target_len].copy_from_slice(&target.as_bytes()[..target_len]);
    path_buf[..path_len].copy_from_slice(&path.as_bytes()[..path_len]);
    send_request(FsRequest::Symlink {
        target: target_buf,
        target_len,
        path: path_buf,
        path_len,
        reply: ReplySlot::new(reply_inner),
    });
    ReplyFuture { inner: reply_inner }.await
}

/// Read symbolic link content.
pub async fn fs_readlink(path: &str) -> Result<(usize, [u8; 256]), Errno> {
    let reply_inner = alloc_readlink_reply();
    let mut path_buf = [0u8; 256];
    let path_len = path.len().min(256);
    path_buf[..path_len].copy_from_slice(&path.as_bytes()[..path_len]);
    send_request(FsRequest::ReadLink {
        path: path_buf,
        path_len,
        reply: ReplySlot::new(reply_inner),
    });
    ReplyFuture { inner: reply_inner }.await
}

/// Flush cache for the filesystem containing `path`.
pub async fn fs_cache_flush(path: &str) -> Result<(), Errno> {
    let reply_inner = alloc_cacheflush_reply();
    let mut path_buf = [0u8; 256];
    let path_len = path.len().min(256);
    path_buf[..path_len].copy_from_slice(&path.as_bytes()[..path_len]);
    send_request(FsRequest::CacheFlush {
        path: path_buf,
        path_len,
        reply: ReplySlot::new(reply_inner),
    });
    ReplyFuture { inner: reply_inner }.await
}

/// Read all directory entries from a directory path.
/// Returns (entries_array, count).
pub async fn fs_readdir(
    path: &str,
    start_idx: usize,
) -> Result<([DirEntryRaw; READDIR_BATCH], usize), Errno> {
    let reply_inner = alloc_readdir_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::ReadDir {
        path: path_buf,
        path_len: len,
        start_idx,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Initialize the delegate: mount ext4 and spawn the delegate task.
pub fn init() {
    let cpu = crate::executor::current().cpu_id;
    crate::executor::spawn_kernel_task(delegate_task(), cpu).detach();
}
