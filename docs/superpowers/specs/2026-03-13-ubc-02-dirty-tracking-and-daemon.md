# UBC Part 2: Dirty Tracking and Page Daemon

**Parent:** `ubc-00-overview.md`

## Existing Infrastructure (stubbed but not wired)

| Component | File:Line | Status |
|-----------|-----------|--------|
| `VmPage.dirty: AtomicU8` | `mm/vm/page.rs:58` | Exists. `set_dirty()`, `is_dirty()`, `clear_dirty_all()` implemented. Never called by write paths. |
| `VmPage.set_valid()` / `is_valid()` | `mm/vm/page.rs:240-250` | Exists. Used by fault path. |
| `VmObject.generation: AtomicU32` | `mm/vm/object.rs:179` | Exists. Initialized to 0. Never read or written. |
| `VmObject.clean_generation: AtomicU32` | `mm/vm/object.rs:180` | Exists. Initialized to 0. Never read or written. |
| `VnodePager::page_out()` | `mm/vm/object.rs:133-148` | Exists. Calls `fs_write_at()`. Never invoked. |
| `VmObject.pages_with_index()` iterator | `mm/vm/object.rs:735` | Exists. Can iterate `(VObjIndex, &Arc<VmPage>)`. |

## New Methods on VmObject

Add to `mm/vm/object.rs`:

```rust
/// Increment modification generation. Called after any page is dirtied.
/// Uses AtomicU32 — no write lock required.
pub fn bump_generation(&self) {
    self.generation.fetch_add(1, Ordering::Release);
}

/// Has any page been modified since last writeback?
pub fn is_dirty(&self) -> bool {
    self.generation.load(Ordering::Acquire)
        != self.clean_generation.load(Ordering::Acquire)
}

/// Snapshot current generation as clean. Called after all dirty pages flushed.
pub fn mark_clean(&self) {
    self.clean_generation.store(
        self.generation.load(Ordering::Acquire),
        Ordering::Release,
    );
}

/// Collect dirty pages for writeback. Returns (page-index, phys-addr) pairs.
/// Caller must hold at least a read lock on this VmObject.
pub fn collect_dirty_pages(&self) -> Vec<(VObjIndex, PhysAddr)> {
    self.pages.iter()
        .filter(|(_, page)| page.is_dirty())
        .map(|(idx, page)| (*idx, page.phys_addr()))
        .collect()
}
```

## Page Daemon

New file: `kernel/src/mm/vm/page_daemon.rs` (~120 lines).

A long-running async kernel task that flushes dirty VmObject pages to disk through `VnodePager::page_out()`.

### Waker Design

```rust
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::task::Waker;

/// Global dirty page counter. Incremented by write path, read by daemon.
static DIRTY_PAGE_COUNT: AtomicU32 = AtomicU32::new(0);

/// High-water mark: wake daemon when dirty count exceeds this.
const DIRTY_HWM: u32 = 64;

/// Daemon waker storage.
static DAEMON_WAKER: IrqSafeSpinLock<Option<Waker>, 6> =
    IrqSafeSpinLock::new(None);
static DAEMON_PENDING: AtomicBool = AtomicBool::new(false);

/// Called by write path after set_dirty().
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
```

### Daemon Future

```rust
/// A future that yields until the daemon is woken.
struct DaemonWait;

impl Future for DaemonWait {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if DAEMON_PENDING.compare_exchange(
            true, false, Ordering::AcqRel, Ordering::Relaxed
        ).is_ok() {
            Poll::Ready(())
        } else {
            *DAEMON_WAKER.lock() = Some(cx.waker().clone());
            // Double-check after storing waker (avoid lost wakeup)
            if DAEMON_PENDING.compare_exchange(
                true, false, Ordering::AcqRel, Ordering::Relaxed
            ).is_ok() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}
```

### Daemon Task

```rust
pub fn spawn_page_daemon() {
    crate::executor::spawn_kernel_task(page_daemon_loop(), 0).detach();
    // Safety net: periodic wakeup
    crate::executor::spawn_kernel_task(async {
        loop {
            crate::executor::sleep(5000).await;
            wake_daemon();
        }
    }, 0).detach();
}

async fn page_daemon_loop() {
    loop {
        DaemonWait.await;

        flush_dirty_vnodes().await;
    }
}

/// Flush all dirty vnode-backed VmObjects. Used by both the daemon and fsync/sync.
pub async fn flush_all_dirty() {
    flush_dirty_vnodes().await;
}

async fn flush_dirty_vnodes() {
    // Snapshot dirty vnodes from VNODE_OBJECTS
    let dirty_vnodes: Vec<(VnodeId, Arc<LeveledRwLock<VmObject, 3>>)> = {
        let cache = VNODE_OBJECTS.lock();
        match cache.as_ref() {
            Some(map) => map.iter()
                .filter(|(_, obj)| obj.read().is_dirty())
                .map(|(id, obj)| (*id, Arc::clone(obj)))
                .collect(),
            None => return,
        }
    };
    // VNODE_OBJECTS lock (level 5) released here

    for (_vnode_id, obj_arc) in dirty_vnodes {
        // Collect dirty pages under read lock
        let (dirty_pages, pager) = {
            let obj = obj_arc.read();
            let pages = obj.collect_dirty_pages();
            let pager = obj.pager.as_ref().map(Arc::clone);
            (pages, pager)
        };
        // VmObject lock (level 3) released here — before I/O

        let Some(pager) = pager else { continue };

        // Flush each dirty page through pager
        for (idx, pa) in &dirty_pages {
            let offset = idx.to_bytes();
            let _ = pager.page_out(offset, *pa).await;
            // → fs_write_at → delegate → lwext4 → disk
        }

        // Clear dirty bits + mark object clean
        {
            let obj = obj_arc.read();
            for (idx, _) in &dirty_pages {
                if let Some(page) = obj.get_page(*idx) {
                    page.clear_dirty_all();
                }
            }
            DIRTY_PAGE_COUNT.fetch_sub(
                dirty_pages.len() as u32, Ordering::Relaxed
            );
            obj.mark_clean();
        }
    }
}
```

### Locking Safety

The daemon follows strict lock ordering:
1. `VNODE_OBJECTS` (level 5) — briefly, to snapshot dirty vnode list
2. VmObject read lock (level 3) — to collect dirty pages and pager ref
3. **Drop VmObject lock** before any I/O
4. `page_out()` → `fs_write_at()` → delegate (level 6) — no VmObject lock held

The daemon never holds a VmObject lock while awaiting I/O. No deadlock possible.

### Boot Integration

In `kernel/src/main.rs`, after the delegate is initialized:

```rust
// After fs::delegate::init() and before user tasks
mm::vm::page_daemon::spawn_page_daemon();
```

### Module Declaration

In `kernel/src/mm/vm/mod.rs`, add:

```rust
pub mod page_daemon;
```

## Files Changed

| File | Change |
|------|--------|
| `kernel/src/mm/vm/page_daemon.rs` | **New file** (~120 lines) |
| `kernel/src/mm/vm/object.rs` | Add `bump_generation`, `is_dirty`, `mark_clean`, `collect_dirty_pages` (~40 lines) |
| `kernel/src/mm/vm/mod.rs` | Add `pub mod page_daemon;` |
| `kernel/src/main.rs` | Call `spawn_page_daemon()` after delegate init (~3 lines) |
