//! TCP socket implementation.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use smoltcp::socket::tcp;

use crate::hal_common::{Errno, KernelResult};
use crate::net::{net_stack, Socket, SocketType};
use crate::net::addr::SockAddrIn4;
use crate::net::util::{get_ephemeral_port, net_block_on};
use crate::proc::Task;

/// Create a new TCP socket and register it with the network stack.
pub fn tcp_create() -> Arc<Socket> {
    let rx_buf = tcp::SocketBuffer::new(alloc::vec![0u8; 65536]);
    let tx_buf = tcp::SocketBuffer::new(alloc::vec![0u8; 65536]);
    let tcp_socket = tcp::Socket::new(rx_buf, tx_buf);
    let handle = net_stack().add_socket(tcp_socket);
    Arc::new(Socket::new(handle, SocketType::Tcp))
}

/// Bind a TCP socket to a local address.
pub fn tcp_bind(sock: &Socket, addr: &SockAddrIn4) -> KernelResult<()> {
    let port = if addr.sin_port == 0 {
        crate::net::util::get_ephemeral_port()
    } else {
        addr.sin_port
    };
    sock.local_port.store(port, Ordering::Relaxed);
    Ok(())
}

/// Start listening on a TCP socket.
pub fn tcp_listen(sock: &Socket) -> KernelResult<()> {
    let port = sock.local_port.load(Ordering::Relaxed);
    if port == 0 {
        return Err(Errno::Einval);
    }
    let stack = net_stack();
    stack.with_socket_mut::<tcp::Socket, _>(sock.handle(), |tcp| {
        tcp.listen(port).map_err(|_| Errno::Eaddrinuse)
    })
}

/// Accept a connection on a listening TCP socket.
///
/// Returns a new connected socket and the remote address.
pub async fn tcp_accept(sock: &Socket, task: &Arc<Task>) -> KernelResult<(Arc<Socket>, SockAddrIn4)> {
    let handle = sock.handle();
    let nonblocking = sock.is_nonblocking();

    // Wait until the listening socket has received a SYN and completed handshake
    net_block_on(nonblocking, handle, true, Some(&task.signals), || {
        let stack = net_stack();
        stack.with_socket::<tcp::Socket, _>(handle, |tcp| {
            if tcp.is_active() {
                Ok(())
            } else {
                Err(Errno::Eagain)
            }
        })
    })
    .await?;

    // Extract the remote endpoint from the now-connected socket
    let remote_ep = net_stack().with_socket::<tcp::Socket, _>(handle, |tcp| {
        tcp.remote_endpoint().ok_or(Errno::Enotconn)
    })?;

    // The original listening socket is now consumed (connected) by smoltcp.
    // Re-create a fresh listening socket on the same port for future accepts.
    let port = sock.local_port.load(Ordering::Relaxed);
    let rx_buf = tcp::SocketBuffer::new(alloc::vec![0u8; 65536]);
    let tx_buf = tcp::SocketBuffer::new(alloc::vec![0u8; 65536]);
    let new_listener = tcp::Socket::new(rx_buf, tx_buf);
    let new_handle = net_stack().add_socket(new_listener);
    net_stack().with_socket_mut::<tcp::Socket, _>(new_handle, |tcp| {
        tcp.listen(port).map_err(|_| Errno::Eaddrinuse)
    })?;

    // Swap: the original sock now owns the new listener handle,
    // and we wrap the old (connected) handle in a new Socket.
    let connected_handle = sock.swap_handle(new_handle);
    let connected = Arc::new(Socket::new(connected_handle, SocketType::Tcp));
    connected
        .local_port
        .store(port, Ordering::Relaxed);

    let addr = SockAddrIn4::from_endpoint(&remote_ep);
    Ok((connected, addr))
}

/// Connect a TCP socket to a remote address.
pub async fn tcp_connect(sock: &Socket, addr: &SockAddrIn4, task: &Arc<Task>) -> KernelResult<()> {
    let remote = addr.to_endpoint();
    let local_port = {
        let p = sock.local_port.load(Ordering::Relaxed);
        if p == 0 {
            let ep = get_ephemeral_port();
            sock.local_port.store(ep, Ordering::Relaxed);
            ep
        } else {
            p
        }
    };

    net_stack().connect_tcp(sock.handle(), remote, local_port)?;

    let handle = sock.handle();
    let nonblocking = sock.is_nonblocking();

    // Wait for connection to complete
    net_block_on(nonblocking, handle, false, Some(&task.signals), || {
        let stack = net_stack();
        stack.with_socket::<tcp::Socket, _>(handle, |tcp| {
            if tcp.may_send() {
                Ok(())
            } else if !tcp.is_active() {
                Err(Errno::Econnrefused)
            } else {
                Err(Errno::Eagain)
            }
        })
    })
    .await
}

/// Send data on a connected TCP socket.
pub async fn tcp_send(sock: &Socket, data: &[u8], task: &Arc<Task>) -> KernelResult<usize> {
    let handle = sock.handle();
    let nonblocking = sock.is_nonblocking();

    net_block_on(nonblocking, handle, false, Some(&task.signals), || {
        let stack = net_stack();
        stack.poll_and_wake();
        stack.with_socket_mut::<tcp::Socket, _>(handle, |tcp| {
            if !tcp.may_send() {
                return Err(Errno::Epipe);
            }
            let n = tcp.send_slice(data).map_err(|_| Errno::Enobufs)?;
            if n == 0 {
                Err(Errno::Eagain)
            } else {
                Ok(n)
            }
        })
    })
    .await
}

/// Receive data from a connected TCP socket.
pub async fn tcp_recv(sock: &Socket, buf: &mut [u8], task: &Arc<Task>) -> KernelResult<usize> {
    let handle = sock.handle();
    let nonblocking = sock.is_nonblocking();

    net_block_on(nonblocking, handle, true, Some(&task.signals), || {
        let stack = net_stack();
        stack.poll_and_wake();
        stack.with_socket_mut::<tcp::Socket, _>(handle, |tcp| {
            if tcp.can_recv() {
                let n = tcp.recv_slice(buf).map_err(|_| Errno::Econnreset)?;
                Ok(n)
            } else if !tcp.may_recv() {
                Ok(0)
            } else {
                Err(Errno::Eagain)
            }
        })
    })
    .await
}

/// Shutdown a TCP socket.
pub fn tcp_shutdown(sock: &Socket) -> KernelResult<()> {
    net_stack().with_socket_mut::<tcp::Socket, _>(sock.handle(), |tcp| {
        tcp.close();
        Ok(())
    })
}

/// Get the local endpoint of a TCP socket.
pub fn tcp_local_endpoint(sock: &Socket) -> KernelResult<SockAddrIn4> {
    let stack = net_stack();
    stack.with_socket::<tcp::Socket, _>(sock.handle(), |tcp| {
        if let Some(ep) = tcp.local_endpoint() {
            Ok(SockAddrIn4::from_endpoint(&ep))
        } else {
            let port = sock.local_port.load(Ordering::Relaxed);
            Ok(SockAddrIn4 {
                sin_family: 2,
                sin_port: port,
                sin_addr: [127, 0, 0, 1],
                sin_zero: [0; 8],
            })
        }
    })
}

/// Get the remote endpoint of a TCP socket.
pub fn tcp_remote_endpoint(sock: &Socket) -> KernelResult<SockAddrIn4> {
    let stack = net_stack();
    stack.with_socket::<tcp::Socket, _>(sock.handle(), |tcp| {
        tcp.remote_endpoint()
            .map(|ep| SockAddrIn4::from_endpoint(&ep))
            .ok_or(Errno::Enotconn)
    })
}
