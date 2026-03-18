//! Network subsystem for rustOS kernel.
//!
//! Architecture: single-lock NetStack wrapping smoltcp Interface + SocketSet,
//! with event-driven waker mechanism for async I/O.

pub mod addr;
pub mod tcp;
pub mod udp;
pub mod util;

use alloc::{collections::BTreeMap, vec};
use core::task::Waker;

use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::Loopback,
    time::Instant,
    wire::{HardwareAddress, IpAddress, IpCidr},
};

use crate::hal_common::{Errno, IrqSafeSpinLock, KernelResult, Once};

use self::util::PollState;

// ---------------------------------------------------------------------------
// Socket: unified socket type exposed to fd_table
// ---------------------------------------------------------------------------

/// Protocol type tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketType {
    Tcp,
    Udp,
}

/// A kernel socket — wraps a smoltcp SocketHandle + metadata.
pub struct Socket {
    pub handle: SocketHandle,
    pub sock_type: SocketType,
    pub nonblocking: core::sync::atomic::AtomicBool,
    /// Bound local port (0 = not bound).
    pub local_port: core::sync::atomic::AtomicU16,
}

impl Socket {
    pub fn new(handle: SocketHandle, sock_type: SocketType) -> Self {
        Self {
            handle,
            sock_type,
            nonblocking: core::sync::atomic::AtomicBool::new(false),
            local_port: core::sync::atomic::AtomicU16::new(0),
        }
    }

    pub fn is_nonblocking(&self) -> bool {
        self.nonblocking.load(core::sync::atomic::Ordering::Relaxed)
    }

    /// Query readiness for ppoll.
    pub fn poll_ready(&self) -> PollState {
        let stack = net_stack();
        match self.sock_type {
            SocketType::Tcp => stack.with_socket::<smoltcp::socket::tcp::Socket, _>(
                self.handle,
                |tcp| PollState {
                    readable: tcp.can_recv(),
                    writable: tcp.can_send(),
                    hangup: !tcp.is_active(),
                },
            ),
            SocketType::Udp => stack.with_socket::<smoltcp::socket::udp::Socket, _>(
                self.handle,
                |udp| PollState {
                    readable: udp.can_recv(),
                    writable: udp.can_send(),
                    hangup: false,
                },
            ),
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let stack = net_stack();
        stack.unregister_waker(self.handle);
        stack.remove_socket(self.handle);
    }
}

// ---------------------------------------------------------------------------
// Global NetStack singleton
// ---------------------------------------------------------------------------

static NET_STACK: Once<NetStack> = Once::new();

/// Get the global network stack. Panics if not initialized.
pub fn net_stack() -> &'static NetStack {
    NET_STACK.get().expect("net: not initialized")
}

/// Initialize the network subsystem (call once during boot).
pub fn init_network() {
    let mut lo_device = Loopback::new(smoltcp::phy::Medium::Ip);

    let config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(config, &mut lo_device, Instant::from_millis(0));

    iface.update_ip_addrs(|addrs| {
        addrs
            .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
            .ok();
    });

    let sockets = SocketSet::new(vec![]);

    NET_STACK.call_once(|| NetStack {
        inner: IrqSafeSpinLock::new(NetStackInner {
            iface,
            device: lo_device,
            sockets,
        }),
        wakers: IrqSafeSpinLock::new(BTreeMap::new()),
    });

    crate::kprintln!("net: loopback interface initialized (127.0.0.1/8)");
}

// ---------------------------------------------------------------------------
// NetStack: single-lock wrapper around smoltcp
// ---------------------------------------------------------------------------

pub struct NetStack {
    inner: IrqSafeSpinLock<NetStackInner, 6>,
    wakers: IrqSafeSpinLock<BTreeMap<SocketHandle, Waker>, 6>,
}

struct NetStackInner {
    iface: Interface,
    device: Loopback,
    sockets: SocketSet<'static>,
}

impl NetStack {
    /// Drive the smoltcp protocol stack and wake any sockets that became ready.
    pub fn poll_and_wake(&self) {
        // 1. Poll smoltcp (loopback needs two polls: TX→buffer, buffer→RX)
        {
            let mut inner = self.inner.lock();
            let ts = Self::now();
            let NetStackInner {
                ref mut iface,
                ref mut device,
                ref mut sockets,
            } = *inner;
            iface.poll(ts, device, sockets);
            iface.poll(ts, device, sockets);
        }

        // 2. Collect wakers to wake (under lock, but don't call wake yet)
        let to_wake: alloc::vec::Vec<Waker> = {
            let inner = self.inner.lock();
            let wakers = self.wakers.lock();
            let mut out = alloc::vec::Vec::new();
            for (&handle, waker) in wakers.iter() {
                let ready = inner
                    .sockets
                    .iter()
                    .find(|(h, _)| *h == handle)
                    .map_or(false, |(_, sock)| match sock {
                        smoltcp::socket::Socket::Tcp(tcp) => {
                            tcp.can_recv() || tcp.can_send() || !tcp.is_active()
                        }
                        smoltcp::socket::Socket::Udp(udp) => udp.can_recv() || udp.can_send(),
                    });
                if ready {
                    out.push(waker.clone());
                }
            }
            out
        };

        // 3. Wake outside of locks
        for w in to_wake {
            w.wake();
        }
    }

    /// Add a socket to the SocketSet, return its handle.
    pub fn add_socket<T: smoltcp::socket::AnySocket<'static>>(&self, socket: T) -> SocketHandle {
        let mut inner = self.inner.lock();
        inner.sockets.add(socket)
    }

    /// Remove a socket from the SocketSet.
    pub fn remove_socket(&self, handle: SocketHandle) {
        let mut inner = self.inner.lock();
        inner.sockets.remove(handle);
    }

    /// Access a socket immutably inside a closure.
    pub fn with_socket<T: smoltcp::socket::AnySocket<'static>, R>(
        &self,
        handle: SocketHandle,
        f: impl FnOnce(&T) -> R,
    ) -> R {
        let inner = self.inner.lock();
        let socket = inner.sockets.get::<T>(handle);
        f(socket)
    }

    /// Access a socket mutably inside a closure.
    pub fn with_socket_mut<T: smoltcp::socket::AnySocket<'static>, R>(
        &self,
        handle: SocketHandle,
        f: impl FnOnce(&mut T) -> R,
    ) -> R {
        let mut inner = self.inner.lock();
        let socket = inner.sockets.get_mut::<T>(handle);
        f(socket)
    }

    /// Initiate a TCP connect (needs iface context for routing).
    pub fn connect_tcp(
        &self,
        handle: SocketHandle,
        remote: smoltcp::wire::IpEndpoint,
        local_port: u16,
    ) -> crate::hal_common::KernelResult<()> {
        let mut inner = self.inner.lock();
        let NetStackInner {
            ref mut iface,
            ref mut sockets,
            ..
        } = *inner;
        let cx = iface.context();
        let socket = sockets.get_mut::<smoltcp::socket::tcp::Socket>(handle);
        socket.set_nagle_enabled(false);
        socket
            .connect(cx, remote, local_port)
            .map_err(|_| crate::hal_common::Errno::Econnrefused)
    }

    /// Register a waker for a socket handle.
    pub fn register_waker(&self, handle: SocketHandle, waker: Waker) {
        self.wakers.lock().insert(handle, waker);
    }

    /// Unregister a waker for a socket handle.
    pub fn unregister_waker(&self, handle: SocketHandle) {
        self.wakers.lock().remove(&handle);
    }

    fn now() -> Instant {
        Instant::from_millis(crate::hal::read_time_ms() as i64)
    }
}
