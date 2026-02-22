---
phase: 02-vm-core-pmap
plan: 02
subsystem: memory
tags: [buddy-allocator, magazine-cache, frame-alloc, memory-poisoning, per-cpu]

requires:
  - phase: 01-boot-executor-hal-foundation
    provides: IrqSafeSpinLock, PerCpu array, executor yield_now
provides:
  - BuddyAllocator with power-of-2 split/coalesce (MAX_ORDER=11)
  - Per-CPU Magazine cache for lock-free order-0 frame alloc/free
  - Dual frame alloc API (frame_alloc async + frame_alloc_sync)
  - Debug-build memory poisoning (0xDEAD_BEEF pattern)
  - emergency_reclaim_sync stub for Phase 5 page daemon
affects: [02-03-pmap, 02-04-asid-fault, 05-page-reclamation]

tech-stack:
  added: []
  patterns: [per-cpu-magazine, buddy-system, dual-sync-async-api, debug-poisoning]

key-files:
  created:
    - kernel/src/mm/allocator/buddy.rs
    - kernel/src/mm/allocator/magazine.rs
    - kernel/src/mm/allocator/frame_allocator.rs
    - kernel/src/mm/allocator/mod.rs
  modified:
    - kernel/src/mm/mod.rs

key-decisions:
  - "PhysAddr-based API (not PhysPageNum) — matches existing hal-common addr types"
  - "IrqSafeSpinLock wrapping per-CPU magazines — ensures IRQ safety even though per-CPU access is mostly uncontended"
  - "Static magazine array indexed by cpu_id — avoids adding field to PerCpu struct (keeps Phase 1 stable)"

patterns-established:
  - "Dual alloc API: frame_alloc_sync() for trap context, frame_alloc() for async context"
  - "Per-CPU magazine pattern: pop/push local, refill/drain batch to global buddy"

requirements-completed: [VM-06, VM-07, VM-08, VM-17]

duration: 4min
completed: 2026-02-23
---

# Phase 2 Plan 2: Buddy Frame Allocator + Per-CPU Magazine Summary
**Buddy system frame allocator with per-CPU magazine cache, dual sync/async alloc API, and debug-build memory poisoning**

## Performance

- **Duration:** 4 min
- **Started:** 2026-02-22T20:36:22Z
- **Completed:** 2026-02-22T20:40:39Z
- **Tasks:** 1
- **Files modified:** 5

## Accomplishments
- Buddy allocator with split/coalesce up to 8MB blocks (MAX_ORDER=11)
- Per-CPU magazine (32 frames) eliminates global lock on single-frame hot path
- frame_alloc_sync() never yields — safe for trap context (sync_fault_handler)
- frame_alloc() async path with yield point for future page daemon cooperation
- Debug-build memory poisoning (0xDEAD_BEEF) catches use-after-free with zero release overhead

## Task Commits

Each task was committed atomically:

1. **Task 1: Buddy allocator + per-CPU magazine + memory poisoning** - `1ffe162` (feat)

## Files Created/Modified
- `kernel/src/mm/allocator/buddy.rs` - BuddyAllocator with free lists per order, split/coalesce, buddy address XOR
- `kernel/src/mm/allocator/magazine.rs` - Per-CPU Magazine with pop/push/refill/drain/drain_half
- `kernel/src/mm/allocator/frame_allocator.rs` - Dual API, init, poisoning, contiguous alloc, emergency reclaim stub
- `kernel/src/mm/allocator/mod.rs` - Module exports for all allocator public API
- `kernel/src/mm/mod.rs` - Added `pub mod allocator` alongside existing `pub mod vm`

## Decisions Made
- Used PhysAddr (not PhysPageNum) to match existing hal-common addr types — no new type needed
- IrqSafeSpinLock wrapping per-CPU magazines for IRQ safety, even though per-CPU access is mostly uncontended
- Static magazine array indexed by cpu_id rather than adding a field to PerCpu struct (keeps Phase 1 stable)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Path correction: os/src/mm/ -> kernel/src/mm/**
- **Found during:** Task 1 (initial file creation)
- **Issue:** Plan referenced `os/src/mm/allocator/` but project uses `kernel/src/`
- **Fix:** Created all files under `kernel/src/mm/allocator/` instead
- **Files modified:** All allocator files
- **Verification:** `cargo build --target riscv64gc-unknown-none-elf -p kernel` succeeds

---

**Total deviations:** 1 auto-fixed (1 blocking path mismatch)
**Impact on plan:** Path correction only, no functional scope change.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Frame allocator ready for PMAP layer (Plan 3) to allocate page table frames
- frame_alloc_sync() ready for sync_fault_handler (Plan 4)
- emergency_reclaim_sync() stubbed for Phase 5 page daemon integration

---
*Phase: 02-vm-core-pmap*
*Completed: 2026-02-23*

## Self-Check: PASSED
