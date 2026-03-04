//! Filesystem delegate: serializes all ext4 operations off the async executor.
//!
//! A single async task owns the lwext4 mount and processes requests
//! from a bounded channel. Callers use async functions (fs_open, fs_read, etc.)
//! that send requests and await replies via oneshot channels.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Poll, Waker};
use core::pin::Pin;

/// FNV-1a hash of a path string, used as synthetic inode number.
fn path_hash(path: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in path.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    // Avoid 0 (invalid inode)
    if h == 0 { 1 } else { h }
}
use core::future::Future;
use hal_common::IrqSafeSpinLock;
use lwext4_rust::Ext4File;

// SAFETY: Ext4File contains raw pointers from lwext4 C code.
// All access is serialized in the single delegate_task — never shared across threads.
struct SendExt4File(Ext4File);
unsafe impl Send for SendExt4File {}

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
        reply: ReplySlot<Result<FsFileHandle, i32>>,
    },
    Read {
        handle: FsFileHandle,
        buf_ptr: usize,
        len: usize,
        reply: ReplySlot<Result<usize, i32>>,
    },
    Write {
        handle: FsFileHandle,
        buf_ptr: usize,
        len: usize,
        reply: ReplySlot<Result<usize, i32>>,
    },
    Close {
        handle: FsFileHandle,
        reply: ReplySlot<Result<(), i32>>,
    },
    /// Look up a child entry in a directory by parent path + name.
    /// Returns (child_ino_unused, file_type, file_size).
    Lookup {
        parent_ino: u32,
        name: [u8; 256],
        name_len: usize,
        reply: ReplySlot<Result<(u32, u8, u64), i32>>,
    },
    /// Stat an inode: returns (size, file_type_u8).
    Stat {
        ino: u32,
        reply: ReplySlot<Result<(u64, u8), i32>>,
    },
    /// Read one page of file data at the given byte offset.
    /// Delegate allocates a frame, reads data into it, returns PhysAddr as usize.
    ReadPage {
        path: [u8; 256],
        path_len: usize,
        offset: u64,
        reply: ReplySlot<Result<usize, i32>>,
    },
    /// Write data to a file at the given byte offset.
    /// Opens file, seeks, writes, closes. Returns bytes written.
    WriteAt {
        path: [u8; 256],
        path_len: usize,
        offset: u64,
        data_ptr: usize,
        data_len: usize,
        reply: ReplySlot<Result<usize, i32>>,
    },
    /// Create a directory at the given path.
    Mkdir {
        path: [u8; 256],
        path_len: usize,
        reply: ReplySlot<Result<(), i32>>,
    },
    /// Remove a file or directory at the given path.
    /// `is_dir`: true for rmdir, false for unlink.
    Unlink {
        path: [u8; 256],
        path_len: usize,
        is_dir: bool,
        reply: ReplySlot<Result<(), i32>>,
    },
    /// Read all directory entries from a directory path.
    /// Returns (entries, count).
    ReadDir {
        path: [u8; 256],
        path_len: usize,
        reply: ReplySlot<Result<([DirEntryRaw; READDIR_BATCH], usize), i32>>,
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
            const INIT: ReplyInner<$T> = ReplyInner {
                done: AtomicBool::new(false),
                waker: IrqSafeSpinLock::new(None),
                value: IrqSafeSpinLock::new(None),
            };
            [INIT; REPLY_POOL_SIZE]
        };
    };
}

define_reply_pool!(OPEN_REPLIES, Result<FsFileHandle, i32>);
define_reply_pool!(READ_REPLIES, Result<usize, i32>);
define_reply_pool!(WRITE_REPLIES, Result<usize, i32>);
define_reply_pool!(CLOSE_REPLIES, Result<(), i32>);
define_reply_pool!(LOOKUP_REPLIES, Result<(u32, u8, u64), i32>);
define_reply_pool!(STAT_REPLIES, Result<(u64, u8), i32>);
define_reply_pool!(READPAGE_REPLIES, Result<usize, i32>);
define_reply_pool!(WRITEAT_REPLIES, Result<usize, i32>);
define_reply_pool!(MKDIR_REPLIES, Result<(), i32>);
define_reply_pool!(UNLINK_REPLIES, Result<(), i32>);
define_reply_pool!(READDIR_REPLIES, Result<([DirEntryRaw; READDIR_BATCH], usize), i32>);

static OPEN_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READ_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static WRITE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static CLOSE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static LOOKUP_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static STAT_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READPAGE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static WRITEAT_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static MKDIR_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static UNLINK_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
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

define_alloc_reply!(alloc_open_reply, OPEN_REPLIES, OPEN_REPLY_IDX, Result<FsFileHandle, i32>);
define_alloc_reply!(alloc_read_reply, READ_REPLIES, READ_REPLY_IDX, Result<usize, i32>);
define_alloc_reply!(alloc_write_reply, WRITE_REPLIES, WRITE_REPLY_IDX, Result<usize, i32>);
define_alloc_reply!(alloc_close_reply, CLOSE_REPLIES, CLOSE_REPLY_IDX, Result<(), i32>);
define_alloc_reply!(alloc_lookup_reply, LOOKUP_REPLIES, LOOKUP_REPLY_IDX, Result<(u32, u8, u64), i32>);
define_alloc_reply!(alloc_stat_reply, STAT_REPLIES, STAT_REPLY_IDX, Result<(u64, u8), i32>);
define_alloc_reply!(alloc_readpage_reply, READPAGE_REPLIES, READPAGE_REPLY_IDX, Result<usize, i32>);
define_alloc_reply!(alloc_writeat_reply, WRITEAT_REPLIES, WRITEAT_REPLY_IDX, Result<usize, i32>);
define_alloc_reply!(alloc_mkdir_reply, MKDIR_REPLIES, MKDIR_REPLY_IDX, Result<(), i32>);
define_alloc_reply!(alloc_unlink_reply, UNLINK_REPLIES, UNLINK_REPLY_IDX, Result<(), i32>);
define_alloc_reply!(alloc_readdir_reply, READDIR_REPLIES, READDIR_REPLY_IDX, Result<([DirEntryRaw; READDIR_BATCH], usize), i32>);

/// Bounded request channel.
static REQUEST_QUEUE: IrqSafeSpinLock<VecDeque<FsRequest>> =
    IrqSafeSpinLock::new(VecDeque::new());
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
        klog!(fs, error, "ext4 mount failed: {}", e);
        return;
    }
    klog!(fs, info, "ext4 mounted, delegate running");

    // SAFETY: We are the single delegate task — the only holder of this token.
    let mut tok = unsafe { crate::fs::ext4::DelegateToken::new() };

    // Open file table: each slot holds an Option<SendExt4File>
    let mut open_files: [Option<SendExt4File>; MAX_OPEN_FILES] = [const { None }; MAX_OPEN_FILES];

    // Process requests forever
    loop {
        let req = DelegateRecvFuture.await;

        match req {
            FsRequest::Open { path, path_len, flags, reply } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                match crate::fs::ext4::open(&mut tok, path_str, flags) {
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
                            None => reply.complete(Err(-24)), // EMFILE
                        }
                    }
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Read { handle, buf_ptr, len, reply } => {
                let idx = handle.0 as usize;
                if idx >= MAX_OPEN_FILES || open_files[idx].is_none() {
                    reply.complete(Err(-9)); // EBADF
                    crate::executor::yield_now().await;
                    continue;
                }
                let file = &mut open_files[idx].as_mut().unwrap().0;
                let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len) };
                match crate::fs::ext4::read(&mut tok, file, buf) {
                    Ok(n) => reply.complete(Ok(n)),
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Write { handle, buf_ptr, len, reply } => {
                let idx = handle.0 as usize;
                if idx >= MAX_OPEN_FILES || open_files[idx].is_none() {
                    reply.complete(Err(-9)); // EBADF
                    crate::executor::yield_now().await;
                    continue;
                }
                let file = &mut open_files[idx].as_mut().unwrap().0;
                let buf = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, len) };
                match crate::fs::ext4::write(&mut tok, file, buf) {
                    Ok(n) => reply.complete(Ok(n)),
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Close { handle, reply } => {
                let idx = handle.0 as usize;
                if idx < MAX_OPEN_FILES {
                    if let Some(ref mut wrapper) = open_files[idx] {
                        let _ = crate::fs::ext4::close(&mut tok, &mut wrapper.0);
                    }
                    open_files[idx] = None;
                }
                reply.complete(Ok(()));
            }
            FsRequest::Lookup { parent_ino: _, name, name_len, reply } => {
                // name is already a full path (e.g. "/bin/init")
                let full_path = core::str::from_utf8(&name[..name_len]).unwrap_or("");
                klog!(fs, debug, "lookup: {:?}", full_path);
                match crate::fs::ext4::stat(&mut tok, full_path) {
                    Ok((size, ftype)) => {
                        // lwext4 doesn't expose raw inodes; use path hash as synthetic ino.
                        let ino = path_hash(full_path);
                        reply.complete(Ok((ino, ftype, size)));
                    }
                    Err(_) => reply.complete(Err(-2)), // ENOENT
                }
            }
            FsRequest::Stat { ino: _, reply } => {
                // Stat not directly usable without a path in lwext4.
                // Callers should use Lookup which returns size+type.
                reply.complete(Err(-38)); // ENOSYS
            }
            FsRequest::ReadPage { path, path_len, offset, reply } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                match crate::mm::allocator::frame_alloc_sync() {
                    Some(pa) => {
                        let buf = unsafe {
                            core::slice::from_raw_parts_mut(pa.as_usize() as *mut u8, 4096)
                        };
                        buf.fill(0);
                        // Open, seek, read, close
                        match crate::fs::ext4::open(&mut tok, path_str, 0) {
                            Ok(mut file) => {
                                let _ = file.file_seek(offset as i64, 0); // SEEK_SET
                                let _ = crate::fs::ext4::read(&mut tok, &mut file, buf);
                                let _ = crate::fs::ext4::close(&mut tok, &mut file);
                                reply.complete(Ok(pa.as_usize()));
                            }
                            Err(e) => {
                                reply.complete(Err(e));
                            }
                        }
                    }
                    None => reply.complete(Err(-12)), // ENOMEM
                }
            }
            FsRequest::WriteAt { path, path_len, offset, data_ptr, data_len, reply } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let data = unsafe { core::slice::from_raw_parts(data_ptr as *const u8, data_len) };
                // Open with write flags, seek, write, close
                match crate::fs::ext4::open(&mut tok, path_str, 0x0002) {
                    Ok(mut file) => {
                        let _ = file.file_seek(offset as i64, 0); // SEEK_SET
                        match crate::fs::ext4::write(&mut tok, &mut file, data) {
                            Ok(n) => {
                                let _ = crate::fs::ext4::close(&mut tok, &mut file);
                                reply.complete(Ok(n));
                            }
                            Err(e) => {
                                let _ = crate::fs::ext4::close(&mut tok, &mut file);
                                reply.complete(Err(e));
                            }
                        }
                    }
                    Err(e) => reply.complete(Err(e)),
                }
            }
            FsRequest::Mkdir { path, path_len, reply } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                reply.complete(crate::fs::ext4::mkdir(&mut tok, path_str).map_err(|e| -(e.abs())));
            }
            FsRequest::Unlink { path, path_len, is_dir, reply } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                let result = if is_dir {
                    // lwext4 dir_rm: use file_remove which handles dirs too
                    crate::fs::ext4::unlink(&mut tok, path_str)
                } else {
                    crate::fs::ext4::unlink(&mut tok, path_str)
                };
                reply.complete(result.map_err(|e| -(e.abs())));
            }
            FsRequest::ReadDir { path, path_len, reply } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                match crate::fs::ext4::dir_open(&mut tok, path_str) {
                    Ok(mut dir) => {
                        let mut entries = [DirEntryRaw {
                            name: [0u8; 255],
                            name_len: 0,
                            inode_type: 0,
                            inode: 0,
                        }; READDIR_BATCH];
                        let mut count = 0usize;
                        while count < READDIR_BATCH {
                            match crate::fs::ext4::dir_next(&mut tok, &mut dir) {
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
                        let _ = crate::fs::ext4::dir_close(&mut tok, &mut dir);
                        reply.complete(Ok((entries, count)));
                    }
                    Err(e) => reply.complete(Err(-(e.abs()))),
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
pub async fn fs_open(path: &str) -> Result<FsFileHandle, i32> {
    fs_open_flags(path, 0).await
}

/// Open a file by path with flags. Returns a file handle.
pub async fn fs_open_flags(path: &str, flags: u32) -> Result<FsFileHandle, i32> {
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
pub async fn fs_read(handle: FsFileHandle, buf: &mut [u8]) -> Result<usize, i32> {
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
pub async fn fs_write(handle: FsFileHandle, buf: &[u8]) -> Result<usize, i32> {
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
pub async fn fs_close(handle: FsFileHandle) -> Result<(), i32> {
    let reply_inner = alloc_close_reply();

    send_request(FsRequest::Close {
        handle,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Look up a child entry in a directory. Returns (child_ino, file_type, file_size).
pub async fn fs_lookup(parent_ino: u32, name: &str) -> Result<(u32, u8, u64), i32> {
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
pub async fn fs_stat(ino: u32) -> Result<(u64, u8), i32> {
    let reply_inner = alloc_stat_reply();

    send_request(FsRequest::Stat {
        ino,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Read one page of file data at the given byte offset.
/// Delegate allocates a frame, reads data into it, returns PhysAddr as usize.
pub async fn fs_read_page(path: &str, offset: u64) -> Result<usize, i32> {
    let reply_inner = alloc_readpage_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::ReadPage {
        path: path_buf,
        path_len: len,
        offset,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Write data to a file at the given byte offset.
/// Opens file, seeks, writes, closes. Returns bytes written.
pub async fn fs_write_at(path: &str, offset: u64, data: &[u8]) -> Result<usize, i32> {
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

/// Create a directory at the given path.
pub async fn fs_mkdir(path: &str) -> Result<(), i32> {
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
pub async fn fs_unlink(path: &str, is_dir: bool) -> Result<(), i32> {
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

/// Read all directory entries from a directory path.
/// Returns (entries_array, count).
pub async fn fs_readdir(path: &str) -> Result<([DirEntryRaw; READDIR_BATCH], usize), i32> {
    let reply_inner = alloc_readdir_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::ReadDir {
        path: path_buf,
        path_len: len,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Initialize the delegate: mount ext4 and spawn the delegate task.
pub fn init() {
    let cpu = crate::executor::per_cpu::current().cpu_id;
    crate::executor::spawn_kernel_task(delegate_task(), cpu).detach();
}
