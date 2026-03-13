# Unified Buffer Cache via VmObject — Design Spec

**Date:** 2026-03-13
**Status:** Approved
**Phase:** 1 (file I/O unification). Phase 2 (pipe page-backing) is out of scope.

## Document Index

| Part | File | Content |
|------|------|---------|
| 0 | `ubc-00-overview.md` | This file — problem statement, architecture, invariants |
| 1 | `ubc-01-write-path.md` | Unified write path through VmObject |
| 2 | `ubc-02-dirty-tracking-and-daemon.md` | Dirty page tracking, page daemon |
| 3 | `ubc-03-fsync-truncation-pagerole.md` | fsync, truncation coherence, PageRole fix |
| 4 | `ubc-04-allocator-visibility.md` | Frame allocator module visibility restriction |

## Problem

The kernel has two independent authorities for file data:

1. **VmObject page cache** — used by `mmap` faults and `sys_read_async`
2. **lwext4 C block cache** — used by `sys_write_async` (heap `Vec<u8>` → delegate → lwext4)

This creates six concrete inconsistencies:

| # | Inconsistency | Location |
|---|---------------|----------|
| 1 | `sys_write` bypasses VmObject. Writes go to lwext4 first, then retroactively patches existing VmObject pages. Window of incoherence between mmap view and disk. | `syscall/fs.rs:1253-1314` |
| 2 | `sys_write` heap-allocates a `Vec<u8>` per write. Unnecessary copy — data should go directly into VmObject pages. | `syscall/fs.rs:1266-1273` |
| 3 | `fsync` flushes only lwext4's block cache. It is unaware of dirty VmObject pages. | `syscall/fs.rs:838-849` |
| 4 | File-backed pages allocated during faults use `PageRole::UserAnon`. Cannot distinguish file cache pages from anonymous pages. | `mm/vm/fault_async.rs:181` |
| 5 | `VmObject.generation` and `clean_generation` fields exist but are never read or written. Dirty tracking infrastructure is stubbed but not wired. | `mm/vm/object.rs:179-180` |
| 6 | Frame allocator functions are all `pub` with `pub mod allocator` — any module can call `alloc_raw_frame_sync()` directly, bypassing VmObject. The doc comment at `allocator/mod.rs:1-11` says not to, but Rust doesn't enforce it. | `mm/allocator/mod.rs:27` |

## Solution

Make VmObject the **single authority** for all file data in the kernel. lwext4 remains the block I/O backend, accessed exclusively through `VnodePager::page_in()` / `page_out()`.

```
                    BEFORE                                    AFTER
                    ──────                                    ─────

mmap fault ──→ VmObject ──→ VnodePager ──→ lwext4   mmap fault ─┐
                                                     read()  ───┤──→ VmObject ──→ VnodePager ──→ lwext4
read()  ──→ VmObject (already correct)               write() ───┘    (single      (page_in /     (block
write() ──→ heap Vec ──→ delegate ──→ lwext4 ──→                      authority)    page_out)       I/O)
             then patch VmObject (if exists)                             │
                                                                   Page Daemon ──→ dirty scan
                                                                                   writeback
                                                                   fsync() ──→ flush dirty pages
```

## Key Invariant

**Once a VmObject exists for a vnode, all data for that vnode lives in VmObject pages.** Reads are lookups + `uiomove()` out. Writes are lookups + `uiomove()` in + mark dirty. Dirty pages are flushed to disk by the page daemon or `fsync()`.

## What Does NOT Change

| Component | Why |
|-----------|-----|
| Read path (`sys_read_async`) | Already goes through VmObject via `fetch_page_async` + `uiomove`. Correct as-is. |
| `mmap` path | Already correct. Uses `vnode_object()` → VmObject → VnodePager. |
| COW / shadow chains | Unaffected. Shadow objects are anonymous overlays on top of vnode-backed objects. |
| lwext4 internal block cache | Remains. Acts as a lower-level block buffer beneath VmObject. Acceptable redundancy. |
| Delegate task architecture | Remains the serialization point for lwext4 I/O. Bottleneck concerns deferred. |
| Pipes | Phase 2. Current inline `[u8; 4096]` ring buffer unchanged. |

## Scope

~350 lines changed/added across 11 files.

| File | Change |
|------|--------|
| `kernel/src/syscall/fs.rs` | Rewrite vnode write path (~80 lines replaced with ~60) |
| `kernel/src/syscall/fs.rs` | Modify fsync (~30 lines added) |
| `kernel/src/syscall/fs.rs` | Modify ftruncate, O_TRUNC (~15 lines added) |
| `kernel/src/mm/vm/object.rs` | Add dirty tracking methods (~40 lines) |
| `kernel/src/mm/vm/fault_async.rs` | Fix PageRole for file-backed pages (~5 lines) |
| `kernel/src/mm/vm/page_daemon.rs` | **New file** — waker-driven page daemon (~120 lines) |
| `kernel/src/mm/vm/mod.rs` | Add `page_daemon` module declaration |
| `kernel/src/main.rs` | Spawn page daemon at boot (~3 lines) |
| `kernel/src/mm/allocator/mod.rs` | Restrict visibility of frame allocator functions |
| `kernel/src/mm/allocator/frame_allocator.rs` | Change `pub` to `pub(super)` on raw allocation functions |
| `kernel/src/mm/mod.rs` | Re-export only legitimate public functions, add `pub(crate)` wrappers for approved external callers |

## Risks

| Risk | Mitigation |
|------|------------|
| Delegate saturation during writeback | Page daemon batches page_out calls with a per-cycle cap. Bottleneck optimization deferred. |
| Write amplification (1-byte write dirties 4KB page) | Standard page-cache behavior. lwext4 handles sub-page writes internally. |
| Power-loss durability | Same as any write-back cache. `fsync`/`fdatasync` guarantee durability. |
| MAP_SHARED multi-process coherence | Already coherent — processes share the same VmObject via `vnode_object()`. |

## Out of Scope

- Phase 2: Pipe page-backing via VmObject
- Replacing lwext4's internal block cache
- Page reclaim / page daemon memory-pressure awareness
- O_DIRECT bypass-cache support
- sendfile / splice zero-copy I/O
