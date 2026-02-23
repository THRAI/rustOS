---
phase: 02-vm-core-pmap
plan: 05
subsystem: testing, vm, pmap, allocator
tags: [unit-tests, qemu-integration, shadow-chain, cow, fault-handler, asid, buddy]

requires:
  - phase: 02-vm-core-pmap
    plan: 01
    provides: "VmObject, VmMap, VmArea data structures"
  - phase: 02-vm-core-pmap
    plan: 02
    provides: "Buddy allocator, magazine, frame_alloc_sync"
  - phase: 02-vm-core-pmap
    plan: 03
    provides: "PMAP layer with Sv39 walk, pmap_enter/extract"
  - phase: 02-vm-core-pmap
    plan: 04
    provides: "ASID allocator, TLB shootdown, sync_fault_handler"
provides:
  - "77 host unit tests covering VM, allocator, PMAP, and ASID logic"
  - "4 QEMU integration tests for fault handler, COW, iterative drop, frame_alloc_sync"
  - "11 total QEMU integration test patterns verified"
affects: [phase-3, testing]

tech-stack:
  added: []
  patterns: ["ASID_TEST_LOCK mutex for parallel test safety on shared global state"]

key-files:
  created:
    - "kernel/src/mm/vm/test_integration.rs"
  modified:
    - "kernel/src/mm/vm/vm_object.rs"
    - "kernel/src/mm/vm/vm_map.rs"
    - "kernel/src/mm/vm/fault.rs"
    - "kernel/src/mm/vm/mod.rs"
    - "kernel/src/mm/allocator/buddy.rs"
    - "kernel-mm/src/pmap_tests.rs"
    - "kernel/src/main.rs"
    - "Makefile"

key-decisions:
  - "ASID tests serialized via std::sync::Mutex to prevent parallel test races on shared global state"
  - "QEMU iterative drop uses 200-deep chain (host tests cover 500+/1000+) to fit within 15s QEMU timeout"
  - "VM integration tests scheduled at 400ms delay to avoid timing conflict with satp-switch test at 200ms"

patterns-established:
  - "VM QEMU integration test pattern: test functions in test_integration.rs, spawned as async tasks in main.rs, output grepped by Makefile"
  - "Parallel-safe test pattern: std::sync::Mutex guard for tests sharing mutable global state"

requirements-completed: [TEST-03]

duration: 8min
completed: 2026-02-23
---

# Plan 05: Host unit tests + QEMU integration tests for Phase 2 VM subsystem

**77 host unit tests and 4 QEMU integration tests verify all Phase 2 success criteria: anonymous fault resolution, COW shadow chains, ASID rollover, frame_alloc_sync safety, and iterative Drop on deep chains.**

## Performance

- **Duration:** 8 min
- **Started:** 2026-02-23T05:42:18Z
- **Completed:** 2026-02-23T05:51:05Z
- **Tasks:** 2
- **Files modified:** 8

## Accomplishments
- Added 19 new host unit tests (58 -> 77 total in kernel-mm) covering VmObject shadow override/depth/600-deep drop, VmMap fork backing chain, buddy coalesce roundtrip, ASID stale detection, and fault handler edge cases
- Created 4 QEMU integration tests verifying anonymous page fault, COW fault, iterative drop, and frame_alloc_sync under real kernel conditions
- Fixed ASID test parallelism race with std::sync::Mutex serialization guard

## Task Commits

Each task was committed atomically:

1. **Task 1: Host unit tests for VM data structures and allocator** - `b44eba3` (test)
2. **Task 2: QEMU integration tests for page faults and ASID** - `13a2c25` (test)

## Files Created/Modified
- `kernel/src/mm/vm/test_integration.rs` - QEMU integration tests for fault handler, COW, iterative drop, frame_alloc_sync
- `kernel/src/mm/vm/vm_object.rs` - 7 new tests: shadow_override, shadow_depth, 600-deep drop, remove_page, backing()
- `kernel/src/mm/vm/vm_map.rs` - 3 new tests: fork backing chain, fork preserves count, invalid range
- `kernel/src/mm/vm/fault.rs` - 3 new tests: unique frames, page-aligned resolution, RWX combos
- `kernel/src/mm/allocator/buddy.rs` - 4 new tests: coalesce roundtrip, split order-10, multiple regions, order too large
- `kernel-mm/src/pmap_tests.rs` - 3 new tests: ASID check_stale, wraps_to_1, max PA encode + parallel safety fix
- `kernel/src/main.rs` - Spawn VM integration test tasks
- `Makefile` - 4 new PASS patterns in qemu-test-rv64

## Deviations

**1. [Auto-fix] ASID test parallelism race**
- **Found during:** Task 1 (host unit tests)
- **Issue:** New ASID tests shared global mutable state with existing tests, causing non-deterministic failures under parallel execution
- **Fix:** Added `ASID_TEST_LOCK: std::sync::Mutex<()>` guard to all 4 ASID tests
- **Verification:** All 77 tests pass in parallel mode

**2. [Auto-fix] QEMU iterative drop depth reduced to 200**
- **Found during:** Task 2 (QEMU integration tests)
- **Issue:** 500-deep shadow chain creation + drop exceeded QEMU 15s timeout
- **Fix:** Reduced to 200-deep on QEMU; host tests still cover 500 and 1000 depths
- **Verification:** All 11 QEMU patterns pass within timeout

## Self-Check: PASSED
- [x] Anonymous page fault resolves to new zeroed frame (host + QEMU)
- [x] COW shadow chain produces private copy on write fault (host + QEMU)
- [x] ASID generation rollover logic verified with stale detection (host)
- [x] frame_alloc_sync() returns frame without yielding (host + QEMU)
- [x] Iterative Drop handles 500+ deep chain without stack overflow (host 1000, QEMU 200)
- [x] No regressions in Phase 1 tests (17 hal-common + all QEMU patterns)

---
*Phase: 02-vm-core-pmap*
*Completed: 2026-02-23*
