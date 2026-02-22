---
phase: 02-vm-core-pmap
plan: 01
subsystem: vm
tags: [vm-object, shadow-chain, cow, btreemap, pmap, iterative-drop]

requires:
  - phase: 01-boot-executor-hal-foundation
    provides: "hal-common crate with PhysAddr, VirtAddr, IrqSafeSpinLock, bump allocator"
provides:
  - "VmObject with shadow chain and iterative Drop"
  - "VmMap with BTreeMap<VirtAddr, VmArea>"
  - "VmArea with monotonic AtomicU64 ID"
  - "PageOwnership enum (Anonymous vs Cached)"
  - "MapPerm bitflags (R/W/X/U)"
  - "fork() COW setup via shadow objects"
affects: [02-vm-core-pmap, 03-fault-handling, 04-process]

tech-stack:
  added: [bitflags]
  patterns: [shadow-chain-cow, iterative-arc-drop, monotonic-id-toctou]

key-files:
  created:
    - kernel/src/mm/vm/vm_object.rs
    - kernel/src/mm/vm/vm_map.rs
    - kernel/src/mm/vm/mod.rs
    - kernel/src/mm/mod.rs
  modified:
    - kernel/src/main.rs
    - kernel/Cargo.toml

key-decisions:
  - "core::mem::take over BTreeMap::drain (drain unstable in no_std nightly)"
  - "bitflags crate for MapPerm instead of manual bit manipulation"
  - "Adapted os/src/mm/vm/ paths to kernel/src/mm/vm/ (project uses kernel/ not os/)"

patterns-established:
  - "Shadow chain pattern: Arc<RwLock<VmObject>> with backing Option for COW"
  - "Iterative Drop via Arc::try_unwrap loop for deep chains"
  - "Monotonic AtomicU64 IDs on VmArea for TOCTOU defense"
  - "BTreeMap range query for VMA lookup by contained address"

requirements-completed: [VM-01, VM-02, VM-03, VM-04, VM-05]

duration: 4min
completed: 2026-02-23
---

# Phase 2 Plan 1: VM Core Data Structures Summary

**BSD-style VmObject with shadow chains and iterative Drop, VmMap with BTreeMap lookup and monotonic VmArea IDs, fork() COW via shadow objects**

## Performance

- **Duration:** 4 min
- **Started:** 2026-02-22T20:35:56Z
- **Completed:** 2026-02-22T20:40:12Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments
- VmObject with BTreeMap<u64, OwnedPage> pages, shadow chain via backing Arc, iterative lookup
- Iterative Drop using Arc::try_unwrap loop — safe for 500+ deep shadow chains
- PageOwnership enum distinguishing Anonymous (freed on drop) from Cached (page cache manages)
- VmMap with BTreeMap<VirtAddr, VmArea> for O(log n) address lookup
- VmArea with monotonic AtomicU64 ID for TOCTOU defense
- fork() creates shadow VmObjects for Anonymous VMAs (COW setup)

## Task Commits

Each task was committed atomically:

1. **Task 1: VmObject with shadow chain, PageOwnership, and iterative Drop** - `fb7b9ca` (feat)
2. **Task 2: VmMap with BTreeMap and VmArea with monotonic ID** - `5efb647` (feat)

## Files Created/Modified
- `kernel/src/mm/vm/vm_object.rs` - VmObject, OwnedPage, PageOwnership, shadow chain, iterative Drop
- `kernel/src/mm/vm/vm_map.rs` - VmMap, VmArea, VmAreaType, VmError, MapPerm, fork()
- `kernel/src/mm/vm/mod.rs` - VM module re-exports
- `kernel/src/mm/mod.rs` - Memory management module root
- `kernel/src/main.rs` - Wire mm module into kernel
- `kernel/Cargo.toml` - Add bitflags dependency

## Decisions Made
- Used `core::mem::take` instead of `BTreeMap::drain` (drain is unstable in no_std nightly)
- Added `bitflags` crate for MapPerm flags instead of manual bit manipulation
- Adapted plan paths from `os/src/mm/vm/` to `kernel/src/mm/vm/` (project uses `kernel/` not `os/`)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Adapted file paths from os/ to kernel/**
- **Found during:** Task 1
- **Issue:** Plan references `os/src/mm/vm/` but project uses `kernel/src/`
- **Fix:** Created all files under `kernel/src/mm/vm/` instead
- **Files modified:** All created files
- **Verification:** Build succeeds
- **Committed in:** fb7b9ca

**2. [Rule 1 - Bug] Replaced BTreeMap::drain with core::mem::take**
- **Found during:** Task 1 (Drop impl)
- **Issue:** `BTreeMap::drain()` is unstable in this nightly toolchain
- **Fix:** Used `core::mem::take(&mut self.pages)` then iterate the owned map
- **Files modified:** kernel/src/mm/vm/vm_object.rs
- **Verification:** Build succeeds
- **Committed in:** fb7b9ca

---

**Total deviations:** 2 auto-fixed (1 blocking, 1 bug)
**Impact on plan:** Both fixes necessary for compilation. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviations above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- VmObject and VmMap are ready for fault handler integration (Plan 3)
- PMAP layer (Plan 3) will wire page table operations to VmMap
- Frame allocator (Plan 2) will provide real frame alloc/free for OwnedPage

---
*Phase: 02-vm-core-pmap*
*Completed: 2026-02-23*

## Self-Check: PASSED

All 4 created files verified on disk. Both task commits (fb7b9ca, 5efb647) verified in git log.
