//! Filesystem delegate: serializes all ext4 operations off the async executor.
//!
//! A single async task owns the Ext4Fs handle and processes requests
//! from a bounded channel. Callers use async functions (fs_open, fs_read, etc.)
//! that send requests and await replies via oneshot channels.
//!
//! Design: per CONTEXT.md, only ONE task ever calls ext4 functions (serialization).
//! The delegate yields between operations so the executor stays responsive.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Poll, Waker};
use core::pin::Pin;
use core::future::Future;
use hal_common::IrqSafeSpinLock;
use crate::kprintln;
use crate::fs::ext4::Ext4Fs;

/// Maximum pending requests in the channel.
const CHANNEL_CAPACITY: usize = 256;

/// Sector size for VirtIO-blk.
const SECTOR_SIZE: usize = 512;

/// File handle (index into delegate's open file table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsFileHandle(pub u16);

/// Filesystem request sent to the delegate.
pub enum FsRequest {
    Open {
        path: [u8; 256],
        path_len: usize,
        reply: ReplySlot<Result<FsFileHandle, i32>>,
    },
    Read {
        handle: FsFileHandle,
        buf_ptr: usize, // physical/virtual address of caller's buffer
        len: usize,
        reply: ReplySlot<Result<usize, i32>>,
    },
    Close {
        handle: FsFileHandle,
        reply: ReplySlot<Result<(), i32>>,
    },
    /// Look up a child entry in a directory by inode + name.
    /// Returns (child_ino, file_type, file_size).
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
        ino: u32,
        offset: u64,
        reply: ReplySlot<Result<usize, i32>>,
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
    /// Create a new reply slot. Caller must ensure the backing storage lives long enough.
    fn new(inner: &'static ReplyInner<T>) -> Self {
        Self { inner }
    }

    /// Set the result and wake the waiter.
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
/// For simplicity, we use a small fixed pool with atomic index.
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
define_reply_pool!(CLOSE_REPLIES, Result<(), i32>);
define_reply_pool!(LOOKUP_REPLIES, Result<(u32, u8, u64), i32>);
define_reply_pool!(STAT_REPLIES, Result<(u64, u8), i32>);
define_reply_pool!(READPAGE_REPLIES, Result<usize, i32>);

static OPEN_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READ_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static CLOSE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static LOOKUP_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static STAT_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);
static READPAGE_REPLY_IDX: AtomicUsize = AtomicUsize::new(0);

fn alloc_open_reply() -> &'static ReplyInner<Result<FsFileHandle, i32>> {
    let idx = OPEN_REPLY_IDX.fetch_add(1, Ordering::Relaxed) % REPLY_POOL_SIZE;
    let r = &OPEN_REPLIES[idx];
    r.done.store(false, Ordering::Relaxed);
    *r.waker.lock() = None;
    *r.value.lock() = None;
    r
}

fn alloc_read_reply() -> &'static ReplyInner<Result<usize, i32>> {
    let idx = READ_REPLY_IDX.fetch_add(1, Ordering::Relaxed) % REPLY_POOL_SIZE;
    let r = &READ_REPLIES[idx];
    r.done.store(false, Ordering::Relaxed);
    *r.waker.lock() = None;
    *r.value.lock() = None;
    r
}

fn alloc_close_reply() -> &'static ReplyInner<Result<(), i32>> {
    let idx = CLOSE_REPLY_IDX.fetch_add(1, Ordering::Relaxed) % REPLY_POOL_SIZE;
    let r = &CLOSE_REPLIES[idx];
    r.done.store(false, Ordering::Relaxed);
    *r.waker.lock() = None;
    *r.value.lock() = None;
    r
}

fn alloc_lookup_reply() -> &'static ReplyInner<Result<(u32, u8, u64), i32>> {
    let idx = LOOKUP_REPLY_IDX.fetch_add(1, Ordering::Relaxed) % REPLY_POOL_SIZE;
    let r = &LOOKUP_REPLIES[idx];
    r.done.store(false, Ordering::Relaxed);
    *r.waker.lock() = None;
    *r.value.lock() = None;
    r
}

fn alloc_stat_reply() -> &'static ReplyInner<Result<(u64, u8), i32>> {
    let idx = STAT_REPLY_IDX.fetch_add(1, Ordering::Relaxed) % REPLY_POOL_SIZE;
    let r = &STAT_REPLIES[idx];
    r.done.store(false, Ordering::Relaxed);
    *r.waker.lock() = None;
    *r.value.lock() = None;
    r
}

fn alloc_readpage_reply() -> &'static ReplyInner<Result<usize, i32>> {
    let idx = READPAGE_REPLY_IDX.fetch_add(1, Ordering::Relaxed) % REPLY_POOL_SIZE;
    let r = &READPAGE_REPLIES[idx];
    r.done.store(false, Ordering::Relaxed);
    *r.waker.lock() = None;
    *r.value.lock() = None;
    r
}

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

/// Block read callback for ext4: reads a filesystem block via VirtIO-blk.
/// Translates filesystem block numbers to sectors.
fn block_read(block_no: u64, buf: &mut [u8]) -> Result<(), ()> {
    let blk = crate::drivers::virtio_blk::get();
    let sectors_per_block = buf.len() / SECTOR_SIZE;
    let start_sector = block_no * sectors_per_block as u64;

    for i in 0..sectors_per_block {
        let sector = start_sector + i as u64;
        let offset = i * SECTOR_SIZE;
        let sector_buf: &mut [u8; 512] = (&mut buf[offset..offset + 512]).try_into().unwrap();
        blk.lock().read_sector(sector, sector_buf)?;
    }
    Ok(())
}

/// Open file table entry in the delegate.
struct OpenFile {
    inode_no: u32,
    offset: usize,
}

/// Maximum open files.
const MAX_OPEN_FILES: usize = 64;

/// Delegate task: the single async task that owns the Ext4Fs and processes requests.
async fn delegate_task() {
    // Mount the ext4 filesystem
    let fs = match Ext4Fs::mount(block_read) {
        Ok(fs) => {
            kprintln!("[fs] ext4 mounted, delegate running");
            fs
        }
        Err(e) => {
            kprintln!("[fs] ext4 mount failed: {}", e);
            return;
        }
    };

    // Open file table
    let mut open_files: [Option<OpenFile>; MAX_OPEN_FILES] = [const { None }; MAX_OPEN_FILES];

    // Process requests forever
    loop {
        // Wait for a request
        let req = DelegateRecvFuture.await;

        match req {
            FsRequest::Open { path, path_len, reply } => {
                let path_str = core::str::from_utf8(&path[..path_len]).unwrap_or("");
                match fs.lookup(path_str) {
                    Ok(ino) => {
                        // Find a free slot
                        let mut slot = None;
                        for (i, f) in open_files.iter().enumerate() {
                            if f.is_none() {
                                slot = Some(i);
                                break;
                            }
                        }
                        match slot {
                            Some(i) => {
                                open_files[i] = Some(OpenFile { inode_no: ino, offset: 0 });
                                reply.complete(Ok(FsFileHandle(i as u16)));
                            }
                            None => reply.complete(Err(-24)), // EMFILE
                        }
                    }
                    Err(_) => reply.complete(Err(-2)), // ENOENT
                }
            }
            FsRequest::Read { handle, buf_ptr, len, reply } => {
                let idx = handle.0 as usize;
                if idx >= MAX_OPEN_FILES || open_files[idx].is_none() {
                    reply.complete(Err(-9)); // EBADF
                    continue;
                }
                let of = open_files[idx].as_mut().unwrap();
                let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len) };
                match fs.read_file(of.inode_no, buf) {
                    Ok(n) => {
                        of.offset += n;
                        reply.complete(Ok(n));
                    }
                    Err(_) => reply.complete(Err(-5)), // EIO
                }
            }
            FsRequest::Close { handle, reply } => {
                let idx = handle.0 as usize;
                if idx < MAX_OPEN_FILES {
                    open_files[idx] = None;
                }
                reply.complete(Ok(()));
            }
            FsRequest::Lookup { parent_ino, name, name_len, reply } => {
                let name_str = core::str::from_utf8(&name[..name_len]).unwrap_or("");
                match fs.lookup_in_dir(parent_ino, name_str) {
                    Ok((child_ino, file_type, file_size)) => {
                        reply.complete(Ok((child_ino, file_type, file_size)));
                    }
                    Err(_) => reply.complete(Err(-2)), // ENOENT
                }
            }
            FsRequest::Stat { ino, reply } => {
                match fs.read_inode(ino) {
                    Ok(inode) => {
                        let ftype = if inode.is_dir() { 2u8 } else { 1u8 };
                        reply.complete(Ok((inode.size(), ftype)));
                    }
                    Err(_) => reply.complete(Err(-2)), // ENOENT
                }
            }
            FsRequest::ReadPage { ino, offset, reply } => {
                // Allocate a frame for the page data
                match crate::mm::allocator::frame_alloc_sync() {
                    Some(pa) => {
                        let buf = unsafe {
                            core::slice::from_raw_parts_mut(pa.as_usize() as *mut u8, 4096)
                        };
                        // Zero the buffer first
                        buf.fill(0);
                        // Read file data at offset into the frame
                        match fs.read_file_at(ino, offset, buf) {
                            Ok(_) => reply.complete(Ok(pa.as_usize())),
                            Err(_) => reply.complete(Err(-5)), // EIO
                        }
                    }
                    None => reply.complete(Err(-12)), // ENOMEM
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
    let reply_inner = alloc_open_reply();
    let mut path_buf = [0u8; 256];
    let len = path.len().min(256);
    path_buf[..len].copy_from_slice(&path.as_bytes()[..len]);

    send_request(FsRequest::Open {
        path: path_buf,
        path_len: len,
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
pub async fn fs_read_page(ino: u32, offset: u64) -> Result<usize, i32> {
    let reply_inner = alloc_readpage_reply();

    send_request(FsRequest::ReadPage {
        ino,
        offset,
        reply: ReplySlot::new(reply_inner),
    });

    ReplyFuture { inner: reply_inner }.await
}

/// Initialize the delegate: mount ext4 and spawn the delegate task.
pub fn init() {
    crate::executor::spawn_kernel_task(delegate_task(), 0).detach();
}
