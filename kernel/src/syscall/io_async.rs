//! Asynchronous I/O system calls.
//!
//! Implements read, write, readv, writev, ioctl, ppoll.

use alloc::string::String;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use hal_common::{Errno, VirtAddr, PAGE_SIZE};

use crate::mm::uio::{uiomove, UioDir};
use crate::mm::vm::fault::PageFaultAccessType;
use crate::mm::vm::fault_async::{page_cache_fetch_by_id, resolve_user_fault};
use crate::proc::task::Task;
use crate::proc::user_copy::fault_in_user_buffer;

/// sys_read_async: read from file descriptor.
pub async fn sys_read_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::fs::fd_table::{DeviceKind, FileObject};

    if len == 0 {
        return Ok(0);
    }

    enum ReadSource {
        Vnode {
            id: u64,
            path: String,
            size: u64,
            offset: u64,
        },
        PipeRead(alloc::sync::Arc<crate::fs::pipe::Pipe>),
        DevNull,
        DevZero,
        DevConsole,
    }

    let (source, desc) = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::EBADF)?;
        if !d.flags.read {
            return Err(Errno::EPERM);
        }
        let src = match &d.object {
            FileObject::Vnode(v) => ReadSource::Vnode {
                id: v.vnode_id(),
                path: String::from(v.path()),
                size: v.size(),
                offset: d.offset.load(core::sync::atomic::Ordering::Relaxed),
            },
            FileObject::PipeRead(p) => ReadSource::PipeRead(Arc::clone(p)),
            FileObject::PipeWrite(_) => return Err(Errno::EBADF),
            FileObject::Device(DeviceKind::Null) => ReadSource::DevNull,
            FileObject::Device(DeviceKind::Zero) => ReadSource::DevZero,
            FileObject::Device(DeviceKind::ConsoleRead) => ReadSource::DevConsole,
            FileObject::Device(DeviceKind::ConsoleWrite) => return Err(Errno::EBADF),
        };
        (src, Arc::clone(d))
    };

    match source {
        ReadSource::DevNull => Ok(0),
        ReadSource::DevZero => {
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::WRITE).await;
            let zeros = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(user_buf as *mut u8, zeros.as_ptr(), len)
            };
            if rc != 0 {
                Err(Errno::EFAULT)
            } else {
                Ok(len)
            }
        }
        ReadSource::DevConsole => {
            ConsoleReadFuture {
                task,
                user_buf,
                len,
            }
            .await
        }
        ReadSource::PipeRead(pipe) => {
            PipeReadFuture {
                pipe,
                task,
                user_buf,
                len,
            }
            .await
        }
        ReadSource::Vnode {
            id,
            path,
            size,
            mut offset,
        } => {
            if offset >= size {
                return Ok(0);
            }
            let to_read = core::cmp::min(len, (size - offset) as usize);
            let mut total = 0usize;

            while total < to_read {
                let page_offset = offset / PAGE_SIZE as u64;
                let offset_in_page = (offset % PAGE_SIZE as u64) as usize;
                let chunk = core::cmp::min(PAGE_SIZE - offset_in_page, to_read - total);

                let pa = page_cache_fetch_by_id(id, &path, page_offset * PAGE_SIZE as u64)
                    .await
                    .map_err(|_| Errno::EIO)?;

                let kern = (pa.as_usize() + offset_in_page) as *mut u8;
                let user = (user_buf + total) as *mut u8;

                match uiomove(kern, user, chunk, UioDir::CopyOut) {
                    Ok(result) => {
                        total += result.done;
                        offset += result.done as u64;
                    }
                    Err(Errno::EFAULT) => {
                        resolve_user_fault(
                            task,
                            VirtAddr::new(user_buf + total),
                            PageFaultAccessType::WRITE,
                        )
                        .await
                        .map_err(|_| Errno::EFAULT)?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            desc.offset
                .store(offset, core::sync::atomic::Ordering::Relaxed);
            Ok(total)
        }
    }
}

/// sys_write_async: write to file descriptor.
pub async fn sys_write_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::fs::fd_table::{DeviceKind, FileObject};

    if len == 0 {
        return Ok(0);
    }

    enum WriteTarget {
        DevNull,
        DevConsole,
        PipeWrite(alloc::sync::Arc<crate::fs::pipe::Pipe>),
    }

    let target = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::EBADF)?;
        if !d.flags.write {
            return Err(Errno::EPERM);
        }
        match &d.object {
            FileObject::Device(DeviceKind::Null) => WriteTarget::DevNull,
            FileObject::Device(DeviceKind::Zero) => WriteTarget::DevNull,
            FileObject::Device(DeviceKind::ConsoleWrite)
            | FileObject::Device(DeviceKind::ConsoleRead) => WriteTarget::DevConsole,
            FileObject::PipeWrite(p) => WriteTarget::PipeWrite(Arc::clone(p)),
            FileObject::PipeRead(_) => return Err(Errno::EBADF),
            FileObject::Vnode(_) => return Err(Errno::EPERM),
        }
    };

    match target {
        WriteTarget::DevNull => Ok(len),
        WriteTarget::DevConsole => {
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::READ).await;
            let mut kbuf = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(
                    kbuf.as_mut_ptr(),
                    user_buf as *const u8,
                    len,
                )
            };
            if rc != 0 {
                return Err(Errno::EFAULT);
            }
            for &b in &kbuf {
                crate::console::putchar(b);
            }
            Ok(len)
        }
        WriteTarget::PipeWrite(pipe) => {
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::READ).await;
            let mut kbuf = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(
                    kbuf.as_mut_ptr(),
                    user_buf as *const u8,
                    len,
                )
            };
            if rc != 0 {
                return Err(Errno::EFAULT);
            }
            let result = PipeWriteFuture {
                pipe,
                task,
                data: kbuf,
                written: 0,
            }
            .await;
            if let Err(Errno::EPIPE) = &result {
                task.signals.post_signal(crate::proc::signal::SIGPIPE);
            }
            result
        }
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
        return Err(Errno::EINVAL);
    }
    if iovcnt == 0 {
        return Ok(0);
    }

    let iov_size = iovcnt * 16;
    fault_in_user_buffer(task, iov_ptr, iov_size, PageFaultAccessType::READ).await;
    let mut iov_buf = alloc::vec![0u8; iov_size];
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            iov_buf.as_mut_ptr(),
            iov_ptr as *const u8,
            iov_size,
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
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
            }
            Err(e) => {
                if total_read > 0 {
                    return Ok(total_read);
                }
                return Err(e);
            }
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
        return Err(Errno::EINVAL);
    }

    let iov_size = iovcnt * 16;
    fault_in_user_buffer(task, iov_ptr, iov_size, PageFaultAccessType::READ).await;
    let mut iov_buf = alloc::vec![0u8; iov_size];
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            iov_buf.as_mut_ptr(),
            iov_ptr as *const u8,
            iov_size,
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
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
            }
            Err(e) => {
                if total > 0 {
                    return Ok(total);
                }
                return Err(e);
            }
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
    use crate::fs::fd_table::{DeviceKind, FileObject};

    let is_console = {
        let tab = task.fd_table.lock();
        let desc = tab.get(fd).ok_or(Errno::EBADF)?;
        matches!(
            &desc.object,
            FileObject::Device(DeviceKind::ConsoleRead)
                | FileObject::Device(DeviceKind::ConsoleWrite)
        )
    };
    if !is_console {
        return Err(Errno::ENOTTY);
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
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        argp as *mut u8,
                        termios.as_ptr() as *const u8,
                        60,
                    )
                };
                if rc != 0 {
                    return Err(Errno::EFAULT);
                }
            }
            Ok(0)
        }
        TIOCGWINSZ => {
            if argp != 0 {
                fault_in_user_buffer(task, argp, 8, PageFaultAccessType::WRITE).await;
                let winsize: [u16; 4] = [24, 80, 0, 0];
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        argp as *mut u8,
                        winsize.as_ptr() as *const u8,
                        8,
                    )
                };
                if rc != 0 {
                    return Err(Errno::EFAULT);
                }
            }
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => Ok(0),
        FIONBIO => Ok(0),
        _ => Err(Errno::ENOTTY),
    }
}

/// sys_ppoll_async: poll file descriptors with timeout.
pub async fn sys_ppoll_async(
    task: &Arc<Task>,
    fds_ptr: usize,
    nfds: usize,
    timeout_ptr: usize,
) -> Result<usize, Errno> {
    use crate::fs::fd_table::{DeviceKind, FileObject};

    if nfds > 256 {
        return Err(Errno::EINVAL);
    }

    let poll_size = nfds * 8;
    if nfds > 0 {
        fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::READ).await;
    }

    let mut poll_buf = alloc::vec![0u8; poll_size];
    if nfds > 0 {
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                poll_buf.as_mut_ptr(),
                fds_ptr as *const u8,
                poll_size,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
    }

    let timeout_ms: Option<u64> = if timeout_ptr != 0 {
        fault_in_user_buffer(task, timeout_ptr, 16, PageFaultAccessType::READ).await;
        let mut ts_buf = [0u8; 16];
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                ts_buf.as_mut_ptr(),
                timeout_ptr as *const u8,
                16,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
        let sec = i64::from_le_bytes(ts_buf[0..8].try_into().unwrap());
        let nsec = i64::from_le_bytes(ts_buf[8..16].try_into().unwrap());
        if sec < 0 || nsec < 0 {
            return Err(Errno::EINVAL);
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
        let now = crate::hal::rv64::timer::read_time_ms();
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
                }
                Some(desc) => match &desc.object {
                    FileObject::Device(DeviceKind::ConsoleRead) => {
                        if events & POLLIN != 0 {
                            revents |= POLLIN;
                        }
                    }
                    FileObject::Device(DeviceKind::ConsoleWrite) => {
                        if events & POLLOUT != 0 {
                            revents |= POLLOUT;
                        }
                    }
                    FileObject::Device(DeviceKind::Null) | FileObject::Device(DeviceKind::Zero) => {
                        revents |= events & (POLLIN | POLLOUT);
                    }
                    FileObject::PipeRead(pipe) => {
                        if pipe.readable_len() > 0 {
                            revents |= POLLIN;
                        }
                        if pipe.is_writer_closed() {
                            revents |= POLLHUP;
                        }
                    }
                    FileObject::PipeWrite(pipe) => {
                        if pipe.is_reader_closed() {
                            revents |= POLLERR;
                        } else if events & POLLOUT != 0 {
                            revents |= POLLOUT;
                        }
                    }
                    FileObject::Vnode(_) => {
                        revents |= events & (POLLIN | POLLOUT);
                    }
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
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        fds_ptr as *mut u8,
                        poll_buf.as_ptr(),
                        poll_size,
                    )
                };
                if rc != 0 {
                    return Err(Errno::EFAULT);
                }
            }
            return Ok(ready_count);
        }

        if let Some(dl) = deadline {
            let now = crate::hal::rv64::timer::read_time_ms();
            if now >= dl {
                if nfds > 0 {
                    fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::WRITE)
                        .await;
                    let rc = unsafe {
                        crate::hal::rv64::copy_user::copy_user_chunk(
                            fds_ptr as *mut u8,
                            poll_buf.as_ptr(),
                            poll_size,
                        )
                    };
                    if rc != 0 {
                        return Err(Errno::EFAULT);
                    }
                }
                return Ok(0);
            }
        }

        if task.signals.has_actionable_pending() {
            return Err(Errno::EINTR);
        }

        crate::executor::sleep(10).await;
    }
}

/// Future for async pipe read.
pub struct PipeReadFuture<'a> {
    pipe: alloc::sync::Arc<crate::fs::pipe::Pipe>,
    task: &'a Arc<Task>,
    user_buf: usize,
    len: usize,
}

impl<'a> Future for PipeReadFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.task.signals.has_actionable_pending() {
            return Poll::Ready(Err(Errno::EINTR));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        match this.pipe.read(&mut kbuf) {
            Ok(0) => Poll::Ready(Ok(0)),
            Ok(n) => {
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        this.user_buf as *mut u8,
                        kbuf.as_ptr(),
                        n,
                    )
                };
                if rc != 0 {
                    Poll::Ready(Err(Errno::EFAULT))
                } else {
                    Poll::Ready(Ok(n))
                }
            }
            Err(Errno::EAGAIN) => {
                this.pipe.register_reader_waker(cx.waker());
                Poll::Pending
            }
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
            return Poll::Ready(Err(Errno::EINTR));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        let n = crate::console::console_read(&mut kbuf);
        if n > 0 {
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(
                    this.user_buf as *mut u8,
                    kbuf.as_ptr(),
                    n,
                )
            };
            if rc != 0 {
                Poll::Ready(Err(Errno::EFAULT))
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
    pipe: alloc::sync::Arc<crate::fs::pipe::Pipe>,
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
            return Poll::Ready(Err(Errno::EINTR));
        }
        loop {
            if this.written >= this.data.len() {
                return Poll::Ready(Ok(this.written));
            }
            match this.pipe.write(&this.data[this.written..]) {
                Ok(n) => {
                    this.written += n;
                    if this.written >= this.data.len() {
                        return Poll::Ready(Ok(this.written));
                    }
                    this.pipe.register_writer_waker(cx.waker());
                    return Poll::Pending;
                }
                Err(Errno::EAGAIN) => {
                    this.pipe.register_writer_waker(cx.waker());
                    return Poll::Pending;
                }
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }
}
