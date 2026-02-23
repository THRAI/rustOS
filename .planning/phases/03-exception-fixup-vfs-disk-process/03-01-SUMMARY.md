---
phase: 03-exception-fixup-vfs-disk-process
plan: 01
subsystem: hal, mm
tags: [pcb_onfault, copy_user, uiomove, exception-fixup, riscv64, assembly]

requires:
  - phase: 02
    provides: "Frame allocator, page tables, trap infrastructure"
provides:
  - "BSD-style pcb_onfault exception fixup on PerCpu"
  - "copy_user_chunk assembly with SUM bit control and landing pad"
  - "uiomove with page-at-a-time chunking and POSIX short-read semantics"
  - "Trap handler fixup for load/store access and page faults"
affects: [03-02, 03-03, 03-04, 03-05]

tech-stack:
  added: []
  patterns: ["pcb_onfault dynamic fixup via tp register", "global_asm with compile-time offset_of", "pluggable copy_fn for host testability"]

key-files:
  created:
    - kernel/src/hal/rv64/copy_user.rs
    - kernel/src/mm/uio.rs
    - kernel-mm/src/uio.rs
  modified:
    - kernel/src/executor/per_cpu.rs
    - kernel/src/hal/rv64/mod.rs
    - kernel/src/hal/rv64/trap.rs
    - kernel/src/main.rs
    - Makefile

key-decisions:
  - "global_asm! with const offset_of!(PerCpu, pcb_onfault) instead of standalone .S file — eliminates offset mismatch bugs"
  - "Trap handler covers both access faults (cause 5,7) and page faults (cause 13,15) — QEMU raises access faults for unmapped addresses"
  - "kernel-mm uio.rs uses pluggable copy_fn parameter for host testability; kernel uio.rs wires in real copy_user_chunk"

patterns-established:
  - "pcb_onfault fixup: assembly sets landing pad address before user memory access, trap handler redirects on fault"
  - "sstatus.SUM bit control: set before user access, clear in both epilogue and landing pad"
  - "Page-at-a-time chunking: chunk_size = min(n, 4096 - (addr & 0xFFF)), never crosses page boundary"

requirements-completed: [HAL-09, HAL-10, HAL-11]

duration: 10min
completed: 2026-02-23
---

# Plan 03-01: Exception Fixup Summary

**BSD-style pcb_onfault exception fixup with copy_user_chunk assembly, uiomove page-chunking, and full trap handler integration — HARD GATE 3a complete.**

## Performance

- **Duration:** 10 min
- **Started:** 2026-02-23T12:17:26Z
- **Completed:** 2026-02-23T12:27:13Z
- **Tasks:** 2
- **Files modified:** 9

## Accomplishments
- Exception fixup works end-to-end: assembly sets pcb_onfault, trap handler redirects to landing pad, EFAULT returned gracefully
- uiomove implements POSIX short-read semantics with page-at-a-time chunking
- 9 new host unit tests (86 total), 3 new QEMU integration tests (14 total), zero regressions

## Task Commits

1. **Task 1: pcb_onfault + copy_user_chunk asm + trap handler fixup** - `f05803d` (feat)
2. **Task 2: uiomove + host tests + QEMU test** - `3d80ecf` (feat)

## Files Created/Modified
- `kernel/src/hal/rv64/copy_user.rs` - copy_user_chunk assembly via global_asm with compile-time PerCpu offset
- `kernel/src/mm/uio.rs` - uiomove with page-at-a-time chunking (kernel-side, wires real assembly)
- `kernel-mm/src/uio.rs` - portable uiomove with pluggable copy_fn + 9 host unit tests
- `kernel/src/executor/per_cpu.rs` - added pcb_onfault AtomicUsize field
- `kernel/src/hal/rv64/trap.rs` - fixup check on access faults (5,7) and page faults (13,15)
- `kernel/src/main.rs` - QEMU integration tests for fixup and uiomove

## Decisions
- Used `global_asm!` with `const offset_of!(PerCpu, pcb_onfault)` instead of standalone `.S` file to eliminate offset mismatch bugs at compile time
- Extended trap handler to cover access faults (cause 5, 7) in addition to page faults (cause 13, 15) — QEMU raises access faults for completely unmapped addresses, page faults only for permission violations
- kernel-mm uio module uses pluggable `copy_fn` parameter for host testability, matching the existing pattern of mirrored test crates

## Deviations
None — followed plan as specified.

## Next Phase Readiness
- HARD GATE 3a is complete — plans 03-02 through 03-05 can now execute
- copy_user_chunk and uiomove are ready for syscall data transfer

---
*Phase: 03-exception-fixup-vfs-disk-process*
*Completed: 2026-02-23*
