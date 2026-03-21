//! Network utility types and helpers.

use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU16, Ordering},
    task::{Context, Poll},
};

use smoltcp::iface::SocketHandle;

use crate::hal_common::{Errno, KernelResult};

// ---------------------------------------------------------------------------
// PollState
// ---------------------------------------------------------------------------

/// Socket readiness state for ppoll integration.
pub struct PollState {
    pub readable: bool,
    pub writable: bool,
    pub hangup: bool,
}

// ---------------------------------------------------------------------------
// Ephemeral port allocation
// ---------------------------------------------------------------------------

static NEXT_PORT: AtomicU16 = AtomicU16::new(49152);

/// Allocate an ephemeral port in the range 49152–65535.
pub fn get_ephemeral_port() -> u16 {
    loop {
        let p = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        if p >= 49152 {
            return p;
        }
        // Wrapped around — reset
        NEXT_PORT.store(49153, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// net_block_on: generic async retry loop
// ---------------------------------------------------------------------------

/// Generic blocking retry for socket operations.
///
/// - `nonblocking=true`: call `f()` once, return EAGAIN on failure.
/// - `nonblocking=false`: loop calling `f()`, yield on EAGAIN, retry.
/// - `want_read=true`: wait for RX ready; `want_read=false`: wait for TX ready.
/// - `signals`: optional reference to the task's signal state; if a pending
///   signal is detected at the start of each retry, returns EINTR so the
///   signal can be delivered before the next blocking attempt.
pub async fn net_block_on<F, T>(
    nonblocking: bool,
    handle: SocketHandle,
    want_read: bool,
    signals: Option<&crate::proc::signal::SignalState>,
    mut f: F,
) -> KernelResult<T>
where
    F: FnMut() -> KernelResult<T>,
{
    if nonblocking {
        return f();
    }
    loop {
        // Return EINTR if a signal became pending (e.g. SIGALRM from setitimer).
        // This allows signal delivery between blocking retries.
        if let Some(sigs) = signals {
            if sigs.has_actionable_pending() {
                return Err(Errno::Eintr);
            }
        }
        super::net_stack().poll_and_wake();
        match f() {
            Ok(val) => return Ok(val),
            Err(Errno::Eagain) => {
                SocketReadyFuture::new(handle, want_read).await;
            }
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// SocketReadyFuture: awaits until a socket might be ready
// ---------------------------------------------------------------------------

struct SocketReadyFuture {
    handle: SocketHandle,
    want_read: bool,
    registered: bool,
}

impl SocketReadyFuture {
    fn new(handle: SocketHandle, want_read: bool) -> Self {
        Self {
            handle,
            want_read,
            registered: false,
        }
    }
}

impl Future for SocketReadyFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let stack = super::net_stack();
        stack.poll_and_wake();

        if self.registered {
            if self.want_read {
                stack.unregister_read_waker(self.handle);
            } else {
                stack.unregister_write_waker(self.handle);
            }
            return Poll::Ready(());
        }

        self.registered = true;
        if self.want_read {
            stack.register_read_waker(self.handle, cx.waker().clone());
        } else {
            stack.register_write_waker(self.handle, cx.waker().clone());
        }
        Poll::Pending
    }
}
