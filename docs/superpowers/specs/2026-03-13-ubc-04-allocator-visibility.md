# UBC Part 4: Frame Allocator Visibility Restriction

**Parent:** `ubc-00-overview.md`

## Problem

The frame allocator's typed wrappers (`alloc_anon_sync`, `alloc_file_cache_sync`, etc.) and raw allocation functions (`alloc_raw_frame_sync`, `frame_alloc_contiguous`) are all `pub` and glob-re-exported via `pub use frame_allocator::*` at `mm/allocator/mod.rs:27`. Any module in the crate can call them directly, bypassing VmObject.

The doc comment at `allocator/mod.rs:1-11` already says:

> **API boundary:** Process-level code (exec, fork) and filesystem code should
> NOT call these functions directly. Instead, use VmObject methods.

But Rust doesn't enforce this — it's a comment, not a visibility restriction.

## Goal

Make the frame allocator **crate-invisible** by default. Expose raw allocation functions only to modules that legitimately need them:

| Legitimate consumer | Functions needed | Why |
|---------------------|-----------------|-----|
| `mm/vm/` (fault handlers, VmObject) | `alloc_raw_frame_sync`, `alloc_anon_sync`, `alloc_file_cache_sync`, `free_raw_frame`, `frame_free` | Core VM — allocates pages for VmObject, page tables |
| `mm/pmap/` | `alloc_pte_l0..l2_sync`, `frame_free` | Page table page allocation |
| `drivers/virtio_blk.rs` | `alloc_raw_frame_sync`, `frame_alloc_contiguous` | DMA buffers — hardware requires raw frames |
| `proc/task.rs` | `frame_alloc_contiguous`, `frame_free_contiguous` | Kernel stack allocation (multi-page) |
| `proc/signal.rs` | `alloc_raw_frame_sync` (`SigTrampoline`) | Signal trampoline page |
| `proc/fork.rs` | `alloc_anon_sync` | COW deep-copy fallback |
| `proc/exec.rs` | `alloc_anon_sync` | User stack for exec |
| `main.rs` | `init_frame_allocator` | Boot-time init |

## Current Access Patterns (8 external call sites)

| # | File | Line | Function | Legitimate? |
|---|------|------|----------|-------------|
| 1 | `proc/signal.rs` | 721 | `alloc_raw_frame_sync(SigTrampoline)` | Yes — trampoline page is a hardware requirement |
| 2 | `proc/fork.rs` | 336 | `alloc_anon_sync()` | Yes — deep-copy fork allocates page-by-page (fallback path) |
| 3 | `proc/exec.rs` | 728 | `alloc_anon_sync()` | Yes — allocates user stack top page eagerly |
| 4 | `proc/task.rs` | 101 | `frame_alloc_contiguous(KSTACK_ORDER)` | Yes — kernel stacks are multi-page, can't go through VmObject |
| 5 | `proc/task.rs` | 205 | `frame_free_contiguous(kstack_base, KSTACK_ORDER)` | Yes — teardown |
| 6 | `drivers/virtio_blk.rs` | 186 | `frame_alloc_contiguous(order)` | Yes — DMA ring buffers |
| 7 | `drivers/virtio_blk.rs` | 212 | `alloc_raw_frame_sync(FileCache)` | Yes — VirtIO request header (DMA). But misuses `PageRole::FileCache` (should be a driver-specific role) |
| 8 | `main.rs` | 174 | `init_frame_allocator(start, end)` | Yes — boot init |

All 8 are legitimate. The goal is not to block these callers but to make the compiler enforce that **no new callers** appear in `syscall/`, `fs/`, or other modules that should go through VmObject.

## Design

### Step 1: Change `frame_allocator.rs` functions from `pub` to `pub(super)`

All allocation/free functions in `frame_allocator.rs` become `pub(super)` — visible only to `mm::allocator` and its parent `mm`:

```rust
// frame_allocator.rs — change all pub to pub(super)
pub(super) fn init_frame_allocator(...) { ... }
pub(super) fn alloc_raw_frame_sync(...) { ... }
pub(super) fn frame_alloc_sync() { ... }
pub(super) async fn alloc_raw_frame(...) { ... }
// ... all macro-generated wrappers too
pub(super) fn frame_free(...) { ... }
pub(super) fn free_raw_frame(...) { ... }
pub(super) fn frame_alloc_contiguous(...) { ... }
pub(super) fn frame_free_contiguous(...) { ... }
pub(super) fn emergency_reclaim_sync() { ... }
pub(super) fn available_pages() { ... }
```

After this, only code inside `mm/` can call them. Everything outside `mm/` gets a compile error.

### Step 2: Change `allocator/mod.rs` glob re-export

Replace the `pub use frame_allocator::*` with a selective `pub(super)` re-export:

```rust
// allocator/mod.rs — replace line 27
// OLD: pub use frame_allocator::*;

// Only re-export for mm-internal use
#[cfg(not(test))]
pub(super) use frame_allocator::*;
```

### Step 3: Create `pub(crate)` gateway functions in `mm/mod.rs`

For the 8 legitimate external callers, expose narrow `pub(crate)` wrappers:

```rust
// mm/mod.rs — replace the current pub use allocator::{...} block

// pub(crate) wrappers for approved external callers:

/// Boot-time initialization. Called only from main.rs.
pub(crate) fn init_frame_allocator(start: PhysAddr, end: PhysAddr) {
    allocator::init_frame_allocator(start, end);
}

/// Allocate a raw frame with a specific role.
pub(crate) fn alloc_raw_frame_sync(role: PageRole) -> Option<PhysAddr> {
    allocator::alloc_raw_frame_sync(role)
}

/// Allocate an anonymous (zero-filled) frame.
pub(crate) fn alloc_anon_sync() -> Option<PhysAddr> {
    allocator::alloc_anon_sync()
}

/// Allocate contiguous frames (multi-page).
pub(crate) fn frame_alloc_contiguous(order: usize) -> Option<PhysAddr> {
    allocator::frame_alloc_contiguous(order)
}

/// Free a single frame.
pub(crate) fn frame_free(frame: &'static mut VmPage) {
    allocator::frame_free(frame);
}

/// Free a raw frame by address.
pub(crate) fn free_raw_frame(addr: PhysAddr) {
    allocator::free_raw_frame(addr);
}

/// Free contiguous frames.
pub(crate) fn frame_free_contiguous(addr: PhysAddr, order: usize) {
    allocator::frame_free_contiguous(addr, order);
}

/// Get frame metadata by physical address.
pub(crate) fn get_frame_meta(addr: PhysAddr) -> Option<&'static mut VmPage> {
    allocator::get_frame_meta(addr)
}

// Types that need to be visible crate-wide
pub use allocator::{BuddyAllocator, Magazine, PageRole};
```

### Step 4: Update external callers

Most external callers already use `crate::mm::alloc_anon_sync()` etc. The only exception:

| Caller | Current path | New path | Change? |
|--------|-------------|----------|---------|
| `proc/exec.rs:728` | `crate::mm::allocator::alloc_anon_sync()` | `crate::mm::alloc_anon_sync()` | Yes — remove `allocator::` |

### Step 5: Fix `PageRole::FileCache` misuse

`drivers/virtio_blk.rs:212` uses `alloc_raw_frame_sync(PageRole::FileCache)` for a VirtIO DMA request header. This is not a file cache page — it's a driver control buffer.

Add `PageRole::DriverDma` to `mm/allocator/types.rs`. Update `virtio_blk.rs` to use it. Reserve `PageRole::FileCache` for actual file cache pages.

## Visibility After Changes

```
                           BEFORE                    AFTER
                           ──────                    ─────
mm/allocator/              pub fn alloc_*            pub(super) fn alloc_*
mm/allocator/mod.rs        pub use fa::*             pub(super) use fa::*
mm/mod.rs                  pub use allocator::{...}  pub(crate) fn alloc_*  (wrappers)
                                                     pub use {PageRole, ...} (types only)
syscall/, fs/              can call alloc_*          COMPILE ERROR
proc/, drivers/, main.rs   can call alloc_*          can call crate::mm::alloc_* (pub(crate))
mm/vm/, mm/pmap/           can call alloc_*          can call alloc_* (inside mm/)
```

## Files Changed

| File | Change |
|------|--------|
| `kernel/src/mm/allocator/frame_allocator.rs` | Change all `pub fn` to `pub(super) fn` |
| `kernel/src/mm/allocator/mod.rs` | Change `pub use frame_allocator::*` to `pub(super) use frame_allocator::*` |
| `kernel/src/mm/mod.rs` | Replace `pub use allocator::{...}` with `pub(crate)` wrapper functions |
| `kernel/src/mm/allocator/types.rs` | Add `PageRole::DriverDma` variant |
| `kernel/src/drivers/virtio_blk.rs` | Change `PageRole::FileCache` to `PageRole::DriverDma` |
| `kernel/src/proc/exec.rs` | Change `crate::mm::allocator::alloc_anon_sync()` to `crate::mm::alloc_anon_sync()` |

## Compiler Enforcement

After these changes, if anyone writes `crate::mm::allocator::alloc_raw_frame_sync(...)` from outside `mm/`, they get:

```
error[E0603]: function `alloc_raw_frame_sync` is private
 --> src/syscall/fs.rs:1234:25
  |
  = note: the function `alloc_raw_frame_sync` is defined here with visibility `pub(super)`
```

The `pub(crate)` wrappers in `mm/mod.rs` serve as the gatekeeping layer. Adding a new wrapper requires explicitly modifying `mm/mod.rs`, creating a visible decision point for code review.
