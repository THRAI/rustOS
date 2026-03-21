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
    wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint},
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
    handle: spin::Mutex<SocketHandle>,
    pub sock_type: SocketType,
    pub nonblocking: core::sync::atomic::AtomicBool,
    /// Bound local port (0 = not bound).
    pub local_port: core::sync::atomic::AtomicU16,
    /// Connected peer endpoint (UDP "connect" stores default remote here).
    pub connected_peer: spin::Mutex<Option<IpEndpoint>>,
}

impl Socket {
    pub fn new(handle: SocketHandle, sock_type: SocketType) -> Self {
        Self {
            handle: spin::Mutex::new(handle),
            sock_type,
            nonblocking: core::sync::atomic::AtomicBool::new(false),
            local_port: core::sync::atomic::AtomicU16::new(0),
            connected_peer: spin::Mutex::new(None),
        }
    }

    /// Get the current socket handle.
    pub fn handle(&self) -> SocketHandle {
        *self.handle.lock()
    }

    /// Swap the socket handle, returning the old one.
    pub fn swap_handle(&self, new_handle: SocketHandle) -> SocketHandle {
        let mut guard = self.handle.lock();
        let old = *guard;
        *guard = new_handle;
        old
    }

    pub fn is_nonblocking(&self) -> bool {
        self.nonblocking.load(core::sync::atomic::Ordering::Relaxed)
    }

    /// Query readiness for ppoll.
    pub fn poll_ready(&self) -> PollState {
        let stack = net_stack();
        let handle = self.handle();
        match self.sock_type {
            SocketType::Tcp => stack.with_socket::<smoltcp::socket::tcp::Socket, _>(
                handle,
                |tcp| PollState {
                    // Report readable on data OR EOF (remote sent FIN → may_recv=false)
                    readable: tcp.can_recv() || !tcp.may_recv(),
                    writable: tcp.can_send(),
                    hangup: !tcp.is_active(),
                },
            ),
            SocketType::Udp => stack.with_socket::<smoltcp::socket::udp::Socket, _>(
                handle,
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
        let handle = self.handle();
        stack.unregister_waker(handle);
        match self.sock_type {
            SocketType::Tcp => {
                // Send FIN and flush any pending TX data before removing.
                let _ = stack.with_socket_mut::<smoltcp::socket::tcp::Socket, _>(handle, |tcp| {
                    tcp.close();
                });
                // Poll several times to give smoltcp a chance to transmit the FIN
                // and any buffered data via the loopback device.
                for _ in 0..8 {
                    stack.poll_and_wake();
                }
            }
            SocketType::Udp => {
                // Flush pending UDP TX data before removing the socket.
                for _ in 0..4 {
                    stack.poll_and_wake();
                }
            }
        }
        stack.remove_socket(handle);
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

/// Spawn a background task that periodically drives the smoltcp stack.
/// Must be called after the executor is initialized.
pub fn spawn_net_poll_task(cpu: usize) {
    crate::executor::spawn_kernel_task(
        async {
            loop {
                net_stack().poll_and_wake();
                crate::executor::sleep(10).await;
            }
        },
        cpu,
    )
    .detach();
    crate::klog!(boot, info, "net: poll task spawned on cpu {}", cpu);
}

// ---------------------------------------------------------------------------
// NetStack: single-lock wrapper around smoltcp
// ---------------------------------------------------------------------------

/// Per-socket waker pair: separate read and write wakers.
#[derive(Default)]
struct SocketWakers {
    read: Option<Waker>,
    write: Option<Waker>,
}

pub struct NetStack {
    inner: IrqSafeSpinLock<NetStackInner, 6>,
    wakers: IrqSafeSpinLock<BTreeMap<SocketHandle, SocketWakers>, 6>,
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
            for (&handle, sw) in wakers.iter() {
                let (rx_ready, tx_ready) = inner
                    .sockets
                    .iter()
                    .find(|(h, _)| *h == handle)
                    .map_or((false, false), |(_, sock)| match sock {
                        smoltcp::socket::Socket::Tcp(tcp) => (
                            tcp.can_recv() || !tcp.is_active(),
                            tcp.can_send() || !tcp.is_active(),
                        ),
                        smoltcp::socket::Socket::Udp(udp) => (udp.can_recv(), udp.can_send()),
                    });
                if rx_ready {
                    if let Some(w) = &sw.read {
                        out.push(w.clone());
                    }
                }
                if tx_ready {
                    if let Some(w) = &sw.write {
                        out.push(w.clone());
                    }
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

    /// Register a read waker for a socket handle.
    pub fn register_read_waker(&self, handle: SocketHandle, waker: Waker) {
        self.wakers.lock().entry(handle).or_default().read = Some(waker);
    }

    /// Register a write waker for a socket handle.
    pub fn register_write_waker(&self, handle: SocketHandle, waker: Waker) {
        self.wakers.lock().entry(handle).or_default().write = Some(waker);
    }

    /// Unregister the read waker for a socket handle.
    pub fn unregister_read_waker(&self, handle: SocketHandle) {
        if let Some(sw) = self.wakers.lock().get_mut(&handle) {
            sw.read = None;
        }
    }

    /// Unregister the write waker for a socket handle.
    pub fn unregister_write_waker(&self, handle: SocketHandle) {
        if let Some(sw) = self.wakers.lock().get_mut(&handle) {
            sw.write = None;
        }
    }

    /// Unregister all wakers for a socket handle (called on socket drop).
    pub fn unregister_waker(&self, handle: SocketHandle) {
        self.wakers.lock().remove(&handle);
    }

    fn now() -> Instant {
        Instant::from_millis(crate::hal::read_time_ms() as i64)
    }
}
