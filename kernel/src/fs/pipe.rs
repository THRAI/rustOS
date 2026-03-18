//! Pipe: 4KB ring buffer with async waker integration.
//!
//! Created by pipe2 syscall. Read/write ends share an Arc<Pipe>.
//! Full implementation in Task 2.

use alloc::sync::Arc;
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
};

use crate::hal_common::{Errno, SpinMutex};

/// Pipe buffer size (also PIPE_BUF for atomic write guarantee).
const PIPE_BUF: usize = 4096;

/// A unidirectional pipe with a fixed ring buffer.
pub struct Pipe {
    buf: SpinMutex<PipeBuffer, 4>,
    /// True when the read end has been closed.
    reader_closed: AtomicBool,
    /// True when the write end has been closed.
    writer_closed: AtomicBool,
}

struct PipeBuffer {
    data: [u8; PIPE_BUF],
    head: usize,
    tail: usize,
    len: usize,
    reader_waker: Option<Waker>,
    writer_waker: Option<Waker>,
}

impl PipeBuffer {
    fn new() -> Self {
        Self {
            data: [0u8; PIPE_BUF],
            head: 0,
            tail: 0,
            len: 0,
            reader_waker: None,
            writer_waker: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn is_full(&self) -> bool {
        self.len == PIPE_BUF
    }

    fn available_read(&self) -> usize {
        self.len
    }

    fn available_write(&self) -> usize {
        PIPE_BUF - self.len
    }
}

impl Pipe {
    /// Create a new pipe. Returns the shared Arc.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            buf: SpinMutex::new(PipeBuffer::new()),
            reader_closed: AtomicBool::new(false),
            writer_closed: AtomicBool::new(false),
        })
    }

    /// Close the read end.
    pub fn close_read(&self) {
        self.reader_closed.store(true, Ordering::Release);
        // Wake writer so it gets EPIPE
        let waker = {
            let mut buf = self.buf.lock();
            buf.writer_waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Close the write end.
    pub fn close_write(&self) {
        self.writer_closed.store(true, Ordering::Release);
        // Wake reader so it gets EOF
        let waker = {
            let mut buf = self.buf.lock();
            buf.reader_waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Read from pipe. Returns Ok(0) on EOF (writer closed + empty).
    /// Returns Err(EAGAIN) if empty and writer alive (caller should register waker and retry).
    pub fn read(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let (n, wake_writer) = {
            let mut buf = self.buf.lock();
            if buf.is_empty() {
                if self.writer_closed.load(Ordering::Acquire) {
                    klog!(pipe, debug, "read EOF");
                    return Ok(0); // EOF
                }
                return Err(Errno::Eagain);
            }
            let to_read = out.len().min(buf.available_read());
            for b in out.iter_mut().take(to_read) {
                let idx = buf.head;
                *b = buf.data[idx];
                buf.head = (idx + 1) % PIPE_BUF;
            }
            buf.len -= to_read;
            let wake = buf.writer_waker.take();
            (to_read, wake)
        };
        if let Some(w) = wake_writer {
            w.wake();
        }
        klog!(pipe, debug, "read {} bytes", n);
        Ok(n)
    }

    /// Write to pipe. Returns Err(EPIPE) if reader closed.
    /// Returns Err(EAGAIN) if full and reader alive.
    pub fn write(&self, data: &[u8]) -> Result<usize, Errno> {
        if self.reader_closed.load(Ordering::Acquire) {
            klog!(pipe, debug, "write EPIPE (reader closed)");
            return Err(Errno::Epipe);
        }
        let (n, wake_reader) = {
            let mut buf = self.buf.lock();
            let avail = buf.available_write();
            if avail == 0 {
                if self.reader_closed.load(Ordering::Acquire) {
                    return Err(Errno::Epipe);
                }
                return Err(Errno::Eagain);
            }
            // PIPE_BUF atomicity: writes <= PIPE_BUF are all-or-nothing
            if data.len() <= PIPE_BUF && avail < data.len() {
                return Err(Errno::Eagain);
            }
            let to_write = data.len().min(avail);
            for &b in data.iter().take(to_write) {
                let idx = buf.tail;
                buf.data[idx] = b;
                buf.tail = (idx + 1) % PIPE_BUF;
            }
            buf.len += to_write;
            let wake = buf.reader_waker.take();
            (to_write, wake)
        };
        if let Some(w) = wake_reader {
            w.wake();
        }
        klog!(pipe, debug, "write {} bytes", n);
        Ok(n)
    }
    pub fn register_reader_waker(&self, waker: &Waker) {
        let mut buf = self.buf.lock();
        buf.reader_waker = Some(waker.clone());
    }

    /// Register a waker for the write end (called when write returns EAGAIN).
    pub fn register_writer_waker(&self, waker: &Waker) {
        let mut buf = self.buf.lock();
        buf.writer_waker = Some(waker.clone());
    }

    pub fn is_reader_closed(&self) -> bool {
        self.reader_closed.load(Ordering::Acquire)
    }

    pub fn is_writer_closed(&self) -> bool {
        self.writer_closed.load(Ordering::Acquire)
    }

    /// How many bytes are available to read without blocking.
    pub fn readable_len(&self) -> usize {
        self.buf.lock().available_read()
    }
}

// ---------------------------------------------------------------------------
// Async pipe IO futures
// ---------------------------------------------------------------------------

use core::{future::Future, pin::Pin, task::Context};

use crate::proc::Task;

/// Future for async pipe read into user memory.
///
/// Handles TOCTOU race: registers waker *first*, then double-checks
/// the pipe, so a close_write() between read()==EAGAIN and waker
/// registration cannot cause a missed wake.
pub struct PipeReadFuture<'a> {
    pub pipe: Arc<Pipe>,
    pub task: &'a Arc<Task>,
    pub user_buf: usize,
    pub len: usize,
}

impl<'a> Future for PipeReadFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> core::task::Poll<Self::Output> {
        let this = self.get_mut();
        if this.task.signals.has_actionable_pending() {
            return core::task::Poll::Ready(Err(Errno::Eintr));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        match this.pipe.read(&mut kbuf) {
            Ok(0) => core::task::Poll::Ready(Ok(0)),
            Ok(n) => {
                let rc = unsafe {
                    crate::hal::copy_user_chunk(this.user_buf as *mut u8, kbuf.as_ptr(), n)
                };
                if rc != 0 {
                    core::task::Poll::Ready(Err(Errno::Efault))
                } else {
                    core::task::Poll::Ready(Ok(n))
                }
            },
            Err(Errno::Eagain) => {
                this.pipe.register_reader_waker(cx.waker());
                let mut kbuf2 = alloc::vec![0u8; this.len];
                match this.pipe.read(&mut kbuf2) {
                    Ok(0) => core::task::Poll::Ready(Ok(0)),
                    Ok(n) => {
                        let rc = unsafe {
                            crate::hal::copy_user_chunk(this.user_buf as *mut u8, kbuf2.as_ptr(), n)
                        };
                        if rc != 0 {
                            core::task::Poll::Ready(Err(Errno::Efault))
                        } else {
                            core::task::Poll::Ready(Ok(n))
                        }
                    },
                    Err(Errno::Eagain) => core::task::Poll::Pending,
                    Err(e) => core::task::Poll::Ready(Err(e)),
                }
            },
            Err(e) => core::task::Poll::Ready(Err(e)),
        }
    }
}

/// Future for async console read (UART input).
pub struct ConsoleReadFuture<'a> {
    pub task: &'a Arc<Task>,
    pub user_buf: usize,
    pub len: usize,
}

impl<'a> Future for ConsoleReadFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> core::task::Poll<Self::Output> {
        let this = self.get_mut();
        if this.task.signals.has_actionable_pending() {
            return core::task::Poll::Ready(Err(Errno::Eintr));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        let n = crate::console::console_read(&mut kbuf);
        if n > 0 {
            let rc =
                unsafe { crate::hal::copy_user_chunk(this.user_buf as *mut u8, kbuf.as_ptr(), n) };
            if rc != 0 {
                core::task::Poll::Ready(Err(Errno::Efault))
            } else {
                core::task::Poll::Ready(Ok(n))
            }
        } else {
            crate::console::console_register_waker(cx.waker());
            core::task::Poll::Pending
        }
    }
}

/// Future for async pipe write from kernel buffer.
///
/// Handles partial writes: if only part of the data fits, the future
/// re-polls until all bytes are written or an error occurs.
pub struct PipeWriteFuture<'a> {
    pub pipe: Arc<Pipe>,
    pub task: &'a Arc<Task>,
    pub data: alloc::vec::Vec<u8>,
    pub written: usize,
}

impl<'a> Future for PipeWriteFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> core::task::Poll<Self::Output> {
        let this = self.get_mut();
        if this.task.signals.has_actionable_pending() {
            if this.written > 0 {
                return core::task::Poll::Ready(Ok(this.written));
            }
            return core::task::Poll::Ready(Err(Errno::Eintr));
        }
        if this.written >= this.data.len() {
            return core::task::Poll::Ready(Ok(this.written));
        }
        match this.pipe.write(&this.data[this.written..]) {
            Ok(n) => {
                this.written += n;
                if this.written >= this.data.len() {
                    return core::task::Poll::Ready(Ok(this.written));
                }
                this.pipe.register_writer_waker(cx.waker());
                match this.pipe.write(&this.data[this.written..]) {
                    Ok(n2) => {
                        this.written += n2;
                        if this.written >= this.data.len() {
                            core::task::Poll::Ready(Ok(this.written))
                        } else {
                            core::task::Poll::Pending
                        }
                    },
                    Err(Errno::Eagain) => core::task::Poll::Pending,
                    Err(Errno::Epipe) => {
                        this.task.signals.post_signal(crate::proc::SIGPIPE);
                        core::task::Poll::Ready(Err(Errno::Epipe))
                    },
                    Err(e) => core::task::Poll::Ready(Err(e)),
                }
            },
            Err(Errno::Eagain) => {
                this.pipe.register_writer_waker(cx.waker());
                match this.pipe.write(&this.data[this.written..]) {
                    Ok(n) => {
                        this.written += n;
                        if this.written >= this.data.len() {
                            core::task::Poll::Ready(Ok(this.written))
                        } else {
                            core::task::Poll::Pending
                        }
                    },
                    Err(Errno::Eagain) => core::task::Poll::Pending,
                    Err(Errno::Epipe) => {
                        this.task.signals.post_signal(crate::proc::SIGPIPE);
                        core::task::Poll::Ready(Err(Errno::Epipe))
                    },
                    Err(e) => core::task::Poll::Ready(Err(e)),
                }
            },
            Err(Errno::Epipe) => {
                this.task.signals.post_signal(crate::proc::SIGPIPE);
                core::task::Poll::Ready(Err(Errno::Epipe))
            },
            Err(e) => core::task::Poll::Ready(Err(e)),
        }
    }
}
