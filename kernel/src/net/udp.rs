//! UDP socket implementation.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use smoltcp::socket::udp;
use smoltcp::wire::IpEndpoint;

use crate::hal_common::{Errno, KernelResult};
use crate::net::{net_stack, Socket, SocketType};
use crate::net::addr::SockAddrIn4;
use crate::net::util::{get_ephemeral_port, net_block_on};

/// Create a new UDP socket and register it with the network stack.
pub fn udp_create() -> Arc<Socket> {
    let rx_meta = udp::PacketMetadata::EMPTY;
    let tx_meta = udp::PacketMetadata::EMPTY;
    let rx_buf = udp::PacketBuffer::new(
        alloc::vec![rx_meta; 64],
        alloc::vec![0u8; 65536],
    );
    let tx_buf = udp::PacketBuffer::new(
        alloc::vec![tx_meta; 64],
        alloc::vec![0u8; 65536],
    );
    let udp_socket = udp::Socket::new(rx_buf, tx_buf);
    let handle = net_stack().add_socket(udp_socket);
    Arc::new(Socket::new(handle, SocketType::Udp))
}

/// Bind a UDP socket to a local address.
pub fn udp_bind(sock: &Socket, addr: &SockAddrIn4) -> KernelResult<()> {
    let ep = addr.to_listen_endpoint();
    sock.local_port.store(addr.sin_port, Ordering::Relaxed);
    net_stack().with_socket_mut::<udp::Socket, _>(sock.handle, |udp| {
        udp.bind(ep).map_err(|_| Errno::Eaddrinuse)
    })
}

/// "Connect" a UDP socket — just stores the default remote endpoint.
/// smoltcp UDP sockets don't have a connect concept, so we store it
/// in the socket metadata and use it as default for send.
///
/// For simplicity, we store the remote endpoint in an atomic-friendly way.
/// Since smoltcp doesn't track this, we use a separate field.
/// For now, we just bind if not already bound.
pub fn udp_connect(sock: &Socket, addr: &SockAddrIn4) -> KernelResult<IpEndpoint> {
    let remote = addr.to_endpoint();

    // Auto-bind if not bound
    let port = sock.local_port.load(Ordering::Relaxed);
    if port == 0 {
        let ep = get_ephemeral_port();
        sock.local_port.store(ep, Ordering::Relaxed);
        let listen_ep = smoltcp::wire::IpListenEndpoint {
            addr: None,
            port: ep,
        };
        net_stack().with_socket_mut::<udp::Socket, _>(sock.handle, |udp| {
            udp.bind(listen_ep).map_err(|_| Errno::Eaddrinuse)
        })?;
    }

    // Store connected peer for sendto with no address
    *sock.connected_peer.lock() = Some(remote);

    Ok(remote)
}

/// Send data to a specific remote address.
pub async fn udp_sendto(
    sock: &Socket,
    data: &[u8],
    remote: &IpEndpoint,
) -> KernelResult<usize> {
    let handle = sock.handle;
    let nonblocking = sock.is_nonblocking();

    // Auto-bind if not bound
    let port = sock.local_port.load(Ordering::Relaxed);
    if port == 0 {
        let ep = get_ephemeral_port();
        sock.local_port.store(ep, Ordering::Relaxed);
        let listen_ep = smoltcp::wire::IpListenEndpoint {
            addr: None,
            port: ep,
        };
        net_stack().with_socket_mut::<udp::Socket, _>(handle, |udp| {
            udp.bind(listen_ep).map_err(|_| Errno::Eaddrinuse)
        })?;
    }

    let remote = *remote;
    net_block_on(nonblocking, handle, || {
        let stack = net_stack();
        stack.poll_and_wake();
        stack.with_socket_mut::<udp::Socket, _>(handle, |udp| {
            udp.send_slice(data, remote)
                .map(|()| data.len())
                .map_err(|_| Errno::Eagain)
        })
    })
    .await
}

/// Receive data from a UDP socket, returning (bytes_read, remote_addr).
pub async fn udp_recvfrom(
    sock: &Socket,
    buf: &mut [u8],
) -> KernelResult<(usize, SockAddrIn4)> {
    let handle = sock.handle;
    let nonblocking = sock.is_nonblocking();

    net_block_on(nonblocking, handle, || {
        let stack = net_stack();
        stack.poll_and_wake();
        stack.with_socket_mut::<udp::Socket, _>(handle, |udp| {
            match udp.recv_slice(buf) {
                Ok((n, meta)) => {
                    let addr = SockAddrIn4::from_endpoint(&meta.endpoint);
                    Ok((n, addr))
                }
                Err(udp::RecvError::Exhausted) => Err(Errno::Eagain),
                Err(udp::RecvError::Truncated) => Err(Errno::Enomem),
            }
        })
    })
    .await
}

/// Get the local endpoint of a UDP socket.
pub fn udp_local_endpoint(sock: &Socket) -> KernelResult<SockAddrIn4> {
    let stack = net_stack();
    stack.with_socket::<udp::Socket, _>(sock.handle, |udp| {
        if let Some(ep) = udp.endpoint().port.checked_sub(0).and_then(|p| {
            if p > 0 {
                Some(smoltcp::wire::IpEndpoint {
                    addr: udp.endpoint().addr.unwrap_or(smoltcp::wire::IpAddress::v4(127, 0, 0, 1)),
                    port: p,
                })
            } else {
                None
            }
        }) {
            Ok(SockAddrIn4::from_endpoint(&ep))
        } else {
            let port = sock.local_port.load(Ordering::Relaxed);
            Ok(SockAddrIn4 {
                sin_family: 2,
                sin_port: port,
                sin_addr: [0, 0, 0, 0],
                sin_zero: [0; 8],
            })
        }
    })
}
