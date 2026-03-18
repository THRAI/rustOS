//! Network system call implementations.

use alloc::sync::Arc;

use crate::{
    fs::fd_table::{FdFlags, FileDescription, FileObject, OpenFlags},
    hal_common::{Errno, KernelResult},
    mm::vm::PageFaultAccessType,
    net::{
        addr::SockAddrIn4,
        tcp, udp, Socket, SocketType,
    },
    proc::{fault_in_user_buffer, Task},
};

// Linux constants
const AF_INET: usize = 2;
const SOCK_STREAM: usize = 1;
const SOCK_DGRAM: usize = 2;
const SOCK_NONBLOCK: usize = 0o4000;
const SOCK_CLOEXEC: usize = 0o2000000;

// Shutdown constants
const SHUT_RD: usize = 0;
const SHUT_WR: usize = 1;
const SHUT_RDWR: usize = 2;

// Socket option levels/names
const SOL_SOCKET: usize = 1;
const SO_REUSEADDR: usize = 2;
const SO_ERROR: usize = 4;
const SO_KEEPALIVE: usize = 9;
const SO_SNDBUF: usize = 7;
const SO_RCVBUF: usize = 8;
const IPPROTO_TCP: usize = 6;
const TCP_NODELAY: usize = 1;

/// Helper: extract Arc<Socket> from fd, or return EBADF/ENOTSOCK.
fn get_socket(task: &Arc<Task>, fd: u32) -> KernelResult<Arc<Socket>> {
    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::Ebadf)?;
    match &desc.object {
        FileObject::Socket(s) => Ok(Arc::clone(s)),
        _ => Err(Errno::Enotsock),
    }
}

/// socket(domain, type, protocol) -> fd
pub fn sys_socket(task: &Arc<Task>, domain: usize, sock_type: usize, _protocol: usize) -> KernelResult<usize> {
    if domain != AF_INET {
        return Err(Errno::Eafnosupport);
    }

    let base_type = sock_type & 0xf;
    let nonblock = (sock_type & SOCK_NONBLOCK) != 0;
    let cloexec = (sock_type & SOCK_CLOEXEC) != 0;

    let socket = match base_type {
        SOCK_STREAM => tcp::tcp_create(),
        SOCK_DGRAM => udp::udp_create(),
        _ => return Err(Errno::Eprototype),
    };

    if nonblock {
        socket
            .nonblocking
            .store(true, core::sync::atomic::Ordering::Relaxed);
    }

    let fd_flags = if cloexec {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };

    let desc = FileDescription::new(FileObject::Socket(socket), OpenFlags::RDWR);
    let fd = task.fd_table.lock().insert(desc, fd_flags)?;
    Ok(fd as usize)
}

/// bind(fd, addr, addrlen) -> 0
pub fn sys_bind(task: &Arc<Task>, fd: usize, addr_ptr: usize, addr_len: usize) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    let addr = SockAddrIn4::from_user(addr_ptr, addr_len)?;

    match sock.sock_type {
        SocketType::Tcp => tcp::tcp_bind(&sock, &addr)?,
        SocketType::Udp => udp::udp_bind(&sock, &addr)?,
    }
    Ok(0)
}

/// listen(fd, backlog) -> 0
pub fn sys_listen(task: &Arc<Task>, fd: usize, _backlog: usize) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    match sock.sock_type {
        SocketType::Tcp => tcp::tcp_listen(&sock)?,
        SocketType::Udp => return Err(Errno::Eopnotsupp),
    }
    Ok(0)
}

/// accept(fd, addr, addrlen) -> new_fd
pub async fn sys_accept(task: &Arc<Task>, fd: usize, addr_ptr: usize, addrlen_ptr: usize) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    match sock.sock_type {
        SocketType::Tcp => {
            let (new_sock, remote_addr) = tcp::tcp_accept(&sock).await?;
            let desc = FileDescription::new(FileObject::Socket(new_sock), OpenFlags::RDWR);
            let new_fd = task.fd_table.lock().insert(desc, FdFlags::empty())?;

            // Write remote address to user if requested
            if addr_ptr != 0 {
                remote_addr.to_user(addr_ptr, addrlen_ptr)?;
            }

            // Re-listen on the original socket's port
            tcp::tcp_listen(&sock)?;

            Ok(new_fd as usize)
        }
        SocketType::Udp => Err(Errno::Eopnotsupp),
    }
}

/// accept4(fd, addr, addrlen, flags) -> new_fd
///
/// Like accept but supports SOCK_CLOEXEC and SOCK_NONBLOCK flags.
pub async fn sys_accept4(
    task: &Arc<Task>,
    fd: usize,
    addr_ptr: usize,
    addrlen_ptr: usize,
    flags: usize,
) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    match sock.sock_type {
        SocketType::Tcp => {
            let (new_sock, remote_addr) = tcp::tcp_accept(&sock).await?;

            if (flags & SOCK_NONBLOCK) != 0 {
                new_sock
                    .nonblocking
                    .store(true, core::sync::atomic::Ordering::Relaxed);
            }

            let fd_flags = if (flags & SOCK_CLOEXEC) != 0 {
                FdFlags::CLOEXEC
            } else {
                FdFlags::empty()
            };

            let desc = FileDescription::new(FileObject::Socket(new_sock), OpenFlags::RDWR);
            let new_fd = task.fd_table.lock().insert(desc, fd_flags)?;

            if addr_ptr != 0 {
                remote_addr.to_user(addr_ptr, addrlen_ptr)?;
            }

            tcp::tcp_listen(&sock)?;

            Ok(new_fd as usize)
        }
        SocketType::Udp => Err(Errno::Eopnotsupp),
    }
}

/// connect(fd, addr, addrlen) -> 0
pub async fn sys_connect(task: &Arc<Task>, fd: usize, addr_ptr: usize, addr_len: usize) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    let addr = SockAddrIn4::from_user(addr_ptr, addr_len)?;

    match sock.sock_type {
        SocketType::Tcp => tcp::tcp_connect(&sock, &addr).await?,
        SocketType::Udp => {
            // UDP "connect" just sets default remote
            let _ = udp::udp_connect(&sock, &addr)?;
        }
    }
    Ok(0)
}

/// sendto(fd, buf, len, flags, dest_addr, addrlen) -> bytes_sent
pub async fn sys_sendto(
    task: &Arc<Task>,
    fd: usize,
    buf_ptr: usize,
    len: usize,
    _flags: usize,
    addr_ptr: usize,
    addr_len: usize,
) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;

    fault_in_user_buffer(task, buf_ptr, len, PageFaultAccessType::READ).await;
    let mut kbuf = alloc::vec![0u8; len];
    let rc = unsafe { crate::hal::copy_user_chunk(kbuf.as_mut_ptr(), buf_ptr as *const u8, len) };
    if rc != 0 {
        return Err(Errno::Efault);
    }

    match sock.sock_type {
        SocketType::Tcp => tcp::tcp_send(&sock, &kbuf).await,
        SocketType::Udp => {
            let remote = if addr_ptr != 0 {
                SockAddrIn4::from_user(addr_ptr, addr_len)?.to_endpoint()
            } else {
                // Use connected peer if available
                sock.connected_peer
                    .lock()
                    .ok_or(Errno::Edestaddrreq)?
            };
            udp::udp_sendto(&sock, &kbuf, &remote).await
        }
    }
}

/// recvfrom(fd, buf, len, flags, src_addr, addrlen) -> bytes_read
pub async fn sys_recvfrom(
    task: &Arc<Task>,
    fd: usize,
    buf_ptr: usize,
    len: usize,
    _flags: usize,
    addr_ptr: usize,
    addrlen_ptr: usize,
) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;

    fault_in_user_buffer(task, buf_ptr, len, PageFaultAccessType::WRITE).await;

    match sock.sock_type {
        SocketType::Tcp => {
            let mut kbuf = alloc::vec![0u8; len];
            let n = tcp::tcp_recv(&sock, &mut kbuf).await?;
            if n > 0 {
                let rc = unsafe {
                    crate::hal::copy_user_chunk(buf_ptr as *mut u8, kbuf.as_ptr(), n)
                };
                if rc != 0 {
                    return Err(Errno::Efault);
                }
            }
            Ok(n)
        }
        SocketType::Udp => {
            let mut kbuf = alloc::vec![0u8; len];
            let (n, remote_addr) = udp::udp_recvfrom(&sock, &mut kbuf).await?;
            if n > 0 {
                let rc = unsafe {
                    crate::hal::copy_user_chunk(buf_ptr as *mut u8, kbuf.as_ptr(), n)
                };
                if rc != 0 {
                    return Err(Errno::Efault);
                }
            }
            if addr_ptr != 0 {
                remote_addr.to_user(addr_ptr, addrlen_ptr)?;
            }
            Ok(n)
        }
    }
}

/// getsockname(fd, addr, addrlen) -> 0
pub fn sys_getsockname(task: &Arc<Task>, fd: usize, addr_ptr: usize, addrlen_ptr: usize) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    let addr = match sock.sock_type {
        SocketType::Tcp => tcp::tcp_local_endpoint(&sock)?,
        SocketType::Udp => udp::udp_local_endpoint(&sock)?,
    };
    addr.to_user(addr_ptr, addrlen_ptr)?;
    Ok(0)
}

/// getpeername(fd, addr, addrlen) -> 0
pub fn sys_getpeername(task: &Arc<Task>, fd: usize, addr_ptr: usize, addrlen_ptr: usize) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    let addr = match sock.sock_type {
        SocketType::Tcp => tcp::tcp_remote_endpoint(&sock)?,
        SocketType::Udp => return Err(Errno::Enotconn),
    };
    addr.to_user(addr_ptr, addrlen_ptr)?;
    Ok(0)
}

/// setsockopt(fd, level, optname, optval, optlen) -> 0
///
/// Stub: accept common options silently.
pub fn sys_setsockopt(
    task: &Arc<Task>,
    fd: usize,
    level: usize,
    optname: usize,
    _optval: usize,
    _optlen: usize,
) -> KernelResult<usize> {
    let _sock = get_socket(task, fd as u32)?;
    match (level, optname) {
        (SOL_SOCKET, SO_REUSEADDR | SO_KEEPALIVE | SO_SNDBUF | SO_RCVBUF) => Ok(0),
        (IPPROTO_TCP, TCP_NODELAY) => Ok(0),
        _ => Err(Errno::Enoprotoopt),
    }
}

/// getsockopt(fd, level, optname, optval, optlen) -> 0
pub fn sys_getsockopt(
    task: &Arc<Task>,
    fd: usize,
    level: usize,
    optname: usize,
    optval: usize,
    optlen_ptr: usize,
) -> KernelResult<usize> {
    let _sock = get_socket(task, fd as u32)?;
    match (level, optname) {
        (SOL_SOCKET, SO_ERROR) => {
            // Return 0 (no error)
            let val = 0u32;
            let rc = unsafe {
                crate::hal::copy_user_chunk(optval as *mut u8, &val as *const u32 as *const u8, 4)
            };
            if rc != 0 {
                return Err(Errno::Efault);
            }
            let len = 4u32;
            let rc = unsafe {
                crate::hal::copy_user_chunk(
                    optlen_ptr as *mut u8,
                    &len as *const u32 as *const u8,
                    4,
                )
            };
            if rc != 0 {
                return Err(Errno::Efault);
            }
            Ok(0)
        }
        (SOL_SOCKET, SO_REUSEADDR | SO_KEEPALIVE | SO_SNDBUF | SO_RCVBUF)
        | (IPPROTO_TCP, TCP_NODELAY) => {
            let val = 0u32;
            let rc = unsafe {
                crate::hal::copy_user_chunk(optval as *mut u8, &val as *const u32 as *const u8, 4)
            };
            if rc != 0 {
                return Err(Errno::Efault);
            }
            let len = 4u32;
            let rc = unsafe {
                crate::hal::copy_user_chunk(
                    optlen_ptr as *mut u8,
                    &len as *const u32 as *const u8,
                    4,
                )
            };
            if rc != 0 {
                return Err(Errno::Efault);
            }
            Ok(0)
        }
        _ => Err(Errno::Enoprotoopt),
    }
}

/// shutdown(fd, how) -> 0
pub fn sys_shutdown(task: &Arc<Task>, fd: usize, how: usize) -> KernelResult<usize> {
    let sock = get_socket(task, fd as u32)?;
    match how {
        SHUT_RD | SHUT_WR | SHUT_RDWR => {}
        _ => return Err(Errno::Einval),
    }
    match sock.sock_type {
        SocketType::Tcp => tcp::tcp_shutdown(&sock)?,
        SocketType::Udp => {} // UDP shutdown is a no-op
    }
    Ok(0)
}

/// sendmsg(fd, msg_ptr, flags) -> bytes_sent
///
/// Minimal implementation: parse msghdr, use first iovec as buffer,
/// extract optional destination address, delegate to sendto path.
pub async fn sys_sendmsg(
    task: &Arc<Task>,
    fd: usize,
    msg_ptr: usize,
    _flags: usize,
) -> KernelResult<usize> {
    if msg_ptr == 0 {
        return Err(Errno::Efault);
    }

    // struct msghdr layout on rv64 (each field 8 bytes):
    //   msg_name(8), msg_namelen(8), msg_iov(8), msg_iovlen(8),
    //   msg_control(8), msg_controllen(8), msg_flags(4+pad)
    fault_in_user_buffer(task, msg_ptr, 56, PageFaultAccessType::READ).await;
    let mut hdr = [0u8; 56];
    let rc = unsafe { crate::hal::copy_user_chunk(hdr.as_mut_ptr(), msg_ptr as *const u8, 56) };
    if rc != 0 {
        return Err(Errno::Efault);
    }

    let name_ptr = usize::from_le_bytes(hdr[0..8].try_into().unwrap());
    let name_len = usize::from_le_bytes(hdr[8..16].try_into().unwrap());
    let iov_ptr = usize::from_le_bytes(hdr[16..24].try_into().unwrap());
    let iov_len = usize::from_le_bytes(hdr[24..32].try_into().unwrap());

    if iov_len == 0 {
        return Ok(0);
    }
    if iov_ptr == 0 {
        return Err(Errno::Efault);
    }

    // Read first iovec (base, len)
    fault_in_user_buffer(task, iov_ptr, 16, PageFaultAccessType::READ).await;
    let mut iov = [0u8; 16];
    let rc = unsafe { crate::hal::copy_user_chunk(iov.as_mut_ptr(), iov_ptr as *const u8, 16) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    let buf_ptr = usize::from_le_bytes(iov[0..8].try_into().unwrap());
    let buf_len = usize::from_le_bytes(iov[8..16].try_into().unwrap());

    // Delegate to sendto
    sys_sendto(task, fd, buf_ptr, buf_len, 0, name_ptr, name_len).await
}

/// recvmsg(fd, msg_ptr, flags) -> bytes_read
///
/// Minimal implementation: parse msghdr, use first iovec as buffer,
/// delegate to recvfrom path, write back source address if requested.
pub async fn sys_recvmsg(
    task: &Arc<Task>,
    fd: usize,
    msg_ptr: usize,
    _flags: usize,
) -> KernelResult<usize> {
    if msg_ptr == 0 {
        return Err(Errno::Efault);
    }

    fault_in_user_buffer(task, msg_ptr, 56, PageFaultAccessType::READ).await;
    let mut hdr = [0u8; 56];
    let rc = unsafe { crate::hal::copy_user_chunk(hdr.as_mut_ptr(), msg_ptr as *const u8, 56) };
    if rc != 0 {
        return Err(Errno::Efault);
    }

    let name_ptr = usize::from_le_bytes(hdr[0..8].try_into().unwrap());
    let name_len_ptr = usize::from_le_bytes(hdr[8..16].try_into().unwrap());
    let iov_ptr = usize::from_le_bytes(hdr[16..24].try_into().unwrap());
    let iov_len = usize::from_le_bytes(hdr[24..32].try_into().unwrap());

    if iov_len == 0 {
        return Ok(0);
    }
    if iov_ptr == 0 {
        return Err(Errno::Efault);
    }

    // Read first iovec
    fault_in_user_buffer(task, iov_ptr, 16, PageFaultAccessType::READ).await;
    let mut iov = [0u8; 16];
    let rc = unsafe { crate::hal::copy_user_chunk(iov.as_mut_ptr(), iov_ptr as *const u8, 16) };
    if rc != 0 {
        return Err(Errno::Efault);
    }
    let buf_ptr = usize::from_le_bytes(iov[0..8].try_into().unwrap());
    let buf_len = usize::from_le_bytes(iov[8..16].try_into().unwrap());

    // Delegate to recvfrom
    sys_recvfrom(task, fd, buf_ptr, buf_len, 0, name_ptr, name_len_ptr).await
}
