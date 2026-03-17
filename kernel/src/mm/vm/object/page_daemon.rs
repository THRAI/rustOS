//! Waker-driven page daemon for dirty VmObject page writeback.
//!
//! The daemon is a long-running async kernel task that flushes dirty
//! VmObject pages to disk through `VnodePager::page_out()`.  It is
//! woken by either:
//!
//! 1. **High-water mark** — when the global dirty page count exceeds
//!    `DIRTY_HWM` (64 pages), the write path calls `maybe_wake_page_daemon()`.
//! 2. **Timer safety net** — a companion task wakes the daemon every 5 seconds
//!    to catch low-volume writes that never hit the high-water mark.

use alloc::{sync::Arc, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    task::{Context, Poll, Waker},
};

use crate::{
    fs::{vnode_object_if_exists, VnodeId},
    hal_common::{IrqSafeSpinLock, LeveledRwLock},
    mm::vm::{object::VmObject, VObjIndex},
};

// ---------------------------------------------------------------------------
// Global dirty-page accounting
// ---------------------------------------------------------------------------

/// Global dirty page counter.  Incremented by the write path after
/// `set_dirty()`, decremented by the daemon after successful writeback.
static DIRTY_PAGE_COUNT: AtomicU32 = AtomicU32::new(0);

/// High-water mark: wake the daemon when the dirty count reaches this.
const DIRTY_HWM: u32 = 64;

/// Daemon waker storage (lock level 6 — filesystem I/O level).
static DAEMON_WAKER: IrqSafeSpinLock<Option<Waker>, 6> = IrqSafeSpinLock::new(None);

/// Fast-path flag: set to `true` when the daemon should run.
static DAEMON_PENDING: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Public API for the write path
// ---------------------------------------------------------------------------

/// Called by the write path after marking a page dirty.
///
/// Increments the global dirty counter and wakes the daemon if the
/// high-water mark is exceeded.
pub fn maybe_wake_page_daemon() {
    let count = DIRTY_PAGE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if count >= DIRTY_HWM {
        wake_daemon();
    }
}

fn wake_daemon() {
    DAEMON_PENDING.store(true, Ordering::Release);
    if let Some(w) = DAEMON_WAKER.lock().take() {
        w.wake();
    }
}

// ---------------------------------------------------------------------------
// Daemon wait future
// ---------------------------------------------------------------------------

/// A future that resolves when the daemon is woken (double-checked locking).
struct DaemonWait;

impl Future for DaemonWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // Fast path: already pending.
        if DAEMON_PENDING
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            return Poll::Ready(());
        }

        // Store waker, then double-check to avoid lost wakeup.
        *DAEMON_WAKER.lock() = Some(cx.waker().clone());

        if DAEMON_PENDING
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

// ---------------------------------------------------------------------------
// Daemon loop
// ---------------------------------------------------------------------------

/// Spawn the page daemon on CPU 0.  Call once during boot after the
/// filesystem delegate has been initialised.
pub fn spawn_page_daemon() {
    // Main daemon loop.
    crate::executor::spawn_kernel_task(
        async {
            loop {
                DaemonWait.await;
                flush_dirty_vnodes().await;
            }
        },
        0,
    )
    .detach();

    // Timer safety net: periodic wakeup every 5 s.
    crate::executor::spawn_kernel_task(
        async {
            loop {
                crate::executor::sleep(5000).await;
                wake_daemon();
            }
        },
        0,
    )
    .detach();
}

// ---------------------------------------------------------------------------
// Flush logic (shared between daemon and fsync / sync)
// ---------------------------------------------------------------------------

/// Flush **all** dirty vnode-backed VmObjects to disk.
///
/// This is the common path used by:
/// - the daemon loop,
/// - `sys_sync_async`,
/// - and any other caller that needs a full writeback.
pub async fn flush_all_dirty() {
    flush_dirty_vnodes().await;
}

/// Flush dirty pages for a **single** vnode.  Used by `sys_fsync_async`.
pub async fn flush_vnode_dirty(vnode_id: VnodeId) {
    let Some(obj_arc) = vnode_object_if_exists(vnode_id) else {
        return;
    };
    flush_object(&obj_arc).await;
}

/// Core flush: iterate all vnode-backed objects, write back dirty pages.
async fn flush_dirty_vnodes() {
    // 1. Snapshot dirty vnodes while holding VNODE_OBJECTS briefly (level 5).
    let dirty_vnodes: Vec<(VnodeId, Arc<LeveledRwLock<VmObject, 3>>)> = {
        let cache = crate::fs::vnode_objects_lock();
        match cache.as_ref() {
            Some(map) => map
                .iter()
                .filter(|(_, obj)| obj.read().is_dirty())
                .map(|(id, obj)| (*id, Arc::clone(obj)))
                .collect(),
            None => return,
        }
    };
    // VNODE_OBJECTS lock released.

    for (_id, obj_arc) in dirty_vnodes {
        flush_object(&obj_arc).await;
    }
}

/// Flush a single VmObject's dirty pages.
async fn flush_object(obj_arc: &Arc<LeveledRwLock<VmObject, 3>>) {
    use crate::mm::vm::SharedBusyGuard;

    // Phase 1: Collect dirty pages + acquire sBusy guards under read lock.
    let (dirty_guards, pager, obj_size) = {
        let obj = obj_arc.read();
        let mut guards = alloc::vec::Vec::new();
        for (idx, page_ref) in obj.pages_with_index() {
            if page_ref.meta().is_dirty() {
                if let Some(guard) = SharedBusyGuard::try_new(page_ref.phys()) {
                    guards.push((idx, guard));
                }
                // If sBusy fails (page is exBusy from fault handler), skip.
                // It will be retried on the next daemon cycle.
            }
        }
        let pager = obj.pager.as_ref().map(Arc::clone);
        let size = obj.size();
        (guards, pager, size)
    };
    // Object lock released. sBusy guards keep pages pinned — collapse()
    // will skip them (checks is_shared_busied after D-2 fix).

    let Some(pager) = pager else { return };
    if dirty_guards.is_empty() {
        return;
    }

    // Phase 2: Write each dirty page through the pager.
    for (idx, guard) in &dirty_guards {
        let offset = idx.to_bytes();
        let len = core::cmp::min(
            crate::hal_common::PAGE_SIZE,
            obj_size.saturating_sub(offset),
        );
        if len > 0 {
            let _ = pager.page_out(offset, guard.phys(), len).await;
        }
    }

    // Phase 3: Clear dirty bits under read lock.
    // Only clear for pages that are still in the object (a concurrent
    // munmap/collapse could have removed them during I/O).
    {
        let obj = obj_arc.read();
        let mut cleared = 0u32;
        for (idx, guard) in &dirty_guards {
            if let Some(pr) = obj.get_page(*idx) {
                // Only clear if this is still the same page we wrote back.
                // (PageRef is move-only, so if it's at the same index, it's
                // the same physical frame — no ABA possible.)
                if pr.phys() == guard.phys() {
                    guard.meta().clear_dirty_all();
                    cleared += 1;
                }
            }
        }
        if cleared > 0 {
            DIRTY_PAGE_COUNT.fetch_sub(cleared, Ordering::Relaxed);
            obj.mark_clean();
        }
    }
    // sBusy guards dropped here — pages unpinned.
}
