---
phase: 01-boot-executor-hal-foundation
plan: 02
subsystem: hal, trap
tags: [rv64, trap, timer, tlb, cache, asm, sbi, no_std]

# Dependency graph
requires:
  - phase: 01-01
    provides: "Boot infrastructure, UART, IRQ ops, hal-common crate"
provides:
  - "__kernel_trap asm: save/restore 32 GPRs + CSRs, call Rust handler"
  - "TrapFrame #[repr(C)] with MI accessors (pc, arg, set_ret_val, sp, ra)"
  - "Trap dispatch: classify scause into timer/external/IPI/syscall/page-fault"
  - "Timer set_oneshot via SBI ecall, 10ms interval, tick counter"
  - "TLB flush_addr/flush_asid/flush_all via sfence.vma"
  - "Cache dcache_wb/dcache_inv/fence_io via fence instructions"
affects: [01-03-PLAN, 01-04-PLAN, phase-02-pmap]

# Tech tracking
tech-stack:
  added: [sbi-timer-ecall, sfence.vma, rv64-trap-asm]
  patterns: [global_asm trap entry, TrapFrame repr(C), SBI ecall for timer]

key-files:
  created:
    - kernel/src/hal/rv64/trap.S
    - kernel/src/hal/rv64/trap.rs
    - kernel/src/hal/rv64/timer.rs
    - kernel/src/hal/rv64/tlb.rs
    - kernel/src/hal/rv64/cache.rs
    - kernel/src/trap.rs
    - hal-common/src/trap_frame.rs
    - kernel/src/alloc_early.rs
  modified:
    - kernel/src/hal/rv64/mod.rs
    - kernel/src/main.rs
    - hal-common/src/lib.rs

key-decisions:
  - "SBI Timer extension (EID 0x54494D45) over legacy ecall for set_timer"
  - "37-field TrapFrame (32 GPRs + sstatus/sepc/scause/stval/kernel_tp = 296 bytes)"
  - "Timer prints every 100 ticks (~1s) to avoid UART flood"
  - "Bump allocator for early boot to unblock hal-common alloc dependency"

patterns-established:
  - "TrapFrame MI accessors: frame.pc(), frame.arg(n), frame.set_ret_val()"
  - "Trap asm includes via global_asm!(include_str!) in main.rs"
  - "SBI ecall pattern for timer with a7=EID, a6=FID"

requirements-completed: [HAL-01, HAL-02, HAL-03, HAL-05, HAL-06, HAL-07]

# Metrics
duration: 6min
completed: 2026-02-22
---

# Phase 1 Plan 02: Trap Entry/Exit + Timer + TLB/Cache Ops Summary

**rv64 kernel trap asm saving 32 GPRs + CSRs, Rust dispatch classifying scause, SBI timer at 10ms intervals, sfence.vma TLB flush, fence cache ops**

## Performance

- **Duration:** 6 min
- **Started:** 2026-02-22T19:09:05Z
- **Completed:** 2026-02-22T19:15:19Z
- **Tasks:** 2
- **Files modified:** 11

## Accomplishments
- __kernel_trap asm saves/restores all 32 GPRs + sstatus/sepc/scause/stval/kernel_tp (296-byte TrapFrame)
- Trap dispatch classifies timer IRQ (cause 5) correctly -- verified on QEMU with periodic ticks
- Timer fires at 10ms intervals via SBI set_timer ecall, tick counter increments monotonically
- TLB flush and cache ops compiled and available for Phase 2 PMAP
- TrapFrame MI accessors (pc, arg, set_ret_val, sp, ra, scause, stval) with compile-time size assert

## Task Commits

Each task was committed atomically:

1. **Task 1: Trap entry/exit asm + TrapFrame + trap dispatch** - `b61dd15` (feat)
2. **Task 2: Timer set_oneshot + TLB flush + cache ops** - `52988e5` (feat)

## Files Created/Modified
- `kernel/src/hal/rv64/trap.S` - __kernel_trap and __user_trap asm vectors
- `kernel/src/hal/rv64/trap.rs` - Trap init (stvec + STIE) and scause dispatch
- `kernel/src/hal/rv64/timer.rs` - SBI timer: init, set_oneshot, handle_timer_irq, read_time
- `kernel/src/hal/rv64/tlb.rs` - flush_addr, flush_asid, flush_all via sfence.vma
- `kernel/src/hal/rv64/cache.rs` - dcache_wb, dcache_inv, fence_io
- `kernel/src/trap.rs` - MI trap module re-exporting arch init
- `hal-common/src/trap_frame.rs` - TrapFrame struct with MI accessors
- `kernel/src/alloc_early.rs` - Bump allocator for early boot

## Decisions Made
- Used SBI Timer extension (EID 0x54494D45, FID 0) over legacy ecall -- modern and forward-compatible
- TrapFrame is 37 fields (296 bytes): 32 GPRs + 4 CSRs + kernel_tp, with compile-time size assert
- Timer status prints every 100 ticks (~1s) to avoid UART flood during normal operation
- Added bump allocator to unblock hal-common's alloc dependency from Plan 03

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added bump allocator for hal-common alloc dependency**
- **Found during:** Task 1 (build verification)
- **Issue:** hal-common was modified by Plan 03 to use `extern crate alloc` (run_queue, timer_wheel). Kernel binary had no global allocator, causing link failure.
- **Fix:** Created kernel/src/alloc_early.rs with a 64KB static bump allocator
- **Files modified:** kernel/src/alloc_early.rs, kernel/src/main.rs
- **Verification:** cargo build succeeds, QEMU boots
- **Committed in:** b61dd15

**2. [Rule 1 - Bug] Added kprintln macro imports in submodules**
- **Found during:** Task 1 (build verification)
- **Issue:** kprintln! macro not in scope in hal/rv64/trap.rs and timer.rs submodules
- **Fix:** Added `use crate::kprintln;` to trap.rs and timer.rs
- **Files modified:** kernel/src/hal/rv64/trap.rs, kernel/src/hal/rv64/timer.rs
- **Verification:** cargo build succeeds
- **Committed in:** b61dd15

---

**Total deviations:** 2 auto-fixed (1 blocking, 1 bug)
**Impact on plan:** Both fixes necessary for compilation. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviations above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Trap infrastructure complete: timer IRQs drive preemption path for Plan 04
- TLB flush ops ready for Phase 2 PMAP implementation
- Cache ops ready for DMA correctness in Phase 2
- TrapFrame MI accessors ready for syscall dispatch in Phase 3

---
*Phase: 01-boot-executor-hal-foundation*
*Completed: 2026-02-22*

## Self-Check: PASSED

All 8 created files verified present. Both task commits (b61dd15, 52988e5) verified in git log.
