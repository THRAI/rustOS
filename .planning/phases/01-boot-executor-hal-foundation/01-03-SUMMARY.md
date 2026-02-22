---
phase: 01-boot-executor-hal-foundation
plan: 03
subsystem: executor, hal
tags: [async-task, run-queue, timer-wheel, irq-safe-spinlock, loom, no_std, rv64]

# Dependency graph
requires:
  - phase: 01-01
    provides: "IrqSafeSpinLock, kprintln!, Cargo workspace, boot infrastructure"
provides:
  - "RunQueue<R> wrapping IrqSafeSpinLock<VecDeque<R>> with push/pop FIFO"
  - "TimerWheel with O(1) insert/expire, 256 slots, 10ms tick"
  - "Per-CPU executor loop polling futures from run queue"
  - "async-task spawn_kernel_task + schedule_fn push to run queue"
  - "sleep/yield_now future APIs"
  - "PerCpu struct with run_queue + timer_wheel + identity"
  - "Tier 1 host tests (17) + Tier 1.5 loom tests (2)"
affects: [01-04-PLAN]

# Tech tracking
tech-stack:
  added: [async-task 4.7 (no_std), spin 0.9, loom 0.7]
  patterns: [per-cpu via tp register, schedule_fn closure, wfi idle with SIE sequence]

key-files:
  created:
    - hal-common/src/run_queue.rs
    - hal-common/src/timer_wheel.rs
    - kernel/src/executor/mod.rs
    - kernel/src/executor/per_cpu.rs
    - kernel/src/executor/schedule.rs
  modified:
    - hal-common/src/lib.rs
    - hal-common/src/irq_lock.rs
    - hal-common/Cargo.toml
    - kernel/Cargo.toml
    - kernel/src/main.rs

key-decisions:
  - "async-task with default-features=false for no_std kernel target"
  - "spin::Once for PerCpu array (bypasses const-init limits of VecDeque/Vec)"
  - "Minimum 1 tick delay in timer wheel insert (prevents slot-0 miss on advance)"

patterns-established:
  - "RunQueue<R> generic over R for host testability (u32 in tests, Runnable in kernel)"
  - "TimerWheel is pure (no lock) -- PerCpu wraps it in IrqSafeSpinLock"
  - "Loom tests use mirrored LoomSpinLock with loom primitives"
  - "executor_loop: pop -> run -> wfi with csrsi/wfi/csrci SIE sequence"

requirements-completed: [BOOT-02, BOOT-03, BOOT-04, BOOT-05, TEST-01, TEST-02]

# Metrics
duration: 7min
completed: 2026-02-23
---

# Phase 1 Plan 03: Executor Core + async-task + Timer Wheel + Host Tests Summary

**Per-CPU async executor with IrqSafeSpinLock run queues, async-task spawn/schedule, O(1) timer wheel, and 19 host+loom tests -- verified on QEMU printing from spawned async future**

## Performance

- **Duration:** 7 min
- **Started:** 2026-02-22T19:08:55Z
- **Completed:** 2026-02-22T19:15:44Z
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- RunQueue and TimerWheel in hal-common with 17 passing host tests + 2 loom tests
- Per-CPU executor loop pops Runnables and polls futures, idles with wfi
- Spawned kernel async future runs on QEMU and prints "hello from async future!"
- Timer IRQs fire at 10ms intervals without crashing the executor

## Task Commits

Each task was committed atomically:

1. **Task 1: Run queue + timer wheel + Tier 1/1.5 tests** - `313a940` (feat)
2. **Task 2: Per-CPU executor loop + async-task spawn + sleep future** - `52988e5` (feat)

## Files Created/Modified
- `hal-common/src/run_queue.rs` - Generic RunQueue<R> wrapping IrqSafeSpinLock<VecDeque<R>>
- `hal-common/src/timer_wheel.rs` - 256-slot timer wheel with O(1) insert/expire/cancel
- `hal-common/src/irq_lock.rs` - Added mutual exclusion thread test + loom tests
- `hal-common/src/lib.rs` - Exports run_queue and timer_wheel modules
- `hal-common/Cargo.toml` - Added loom dependency + feature gate
- `kernel/src/executor/mod.rs` - Executor loop: pop, run, wfi idle
- `kernel/src/executor/per_cpu.rs` - PerCpu struct with spin::Once global array, tp register access
- `kernel/src/executor/schedule.rs` - spawn_kernel_task, sleep, yield_now
- `kernel/Cargo.toml` - Added async-task (no_std) + spin dependencies
- `kernel/src/main.rs` - Wired up PerCpu init, spawn test task, enter executor_loop

## Decisions Made
- Used `async-task` with `default-features = false` for no_std compatibility on rv64 bare-metal target
- Used `spin::Once` for PerCpu global array to bypass const-initialization limits of VecDeque/Vec
- Enforced minimum 1 tick delay in timer wheel insert to prevent slot-0 miss when advance() increments before processing

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed timer wheel zero-delay slot miss**
- **Found during:** Task 1 (timer wheel tests)
- **Issue:** 0ms delay computed 0 ticks, placing entry in slot 0 at deadline_tick=0. advance() increments to tick 1 and processes slot 1, missing the entry.
- **Fix:** Added `core::cmp::max(1, ...)` to ensure minimum 1 tick delay
- **Files modified:** hal-common/src/timer_wheel.rs
- **Verification:** zero_delay_expires_on_next_tick test passes
- **Committed in:** 313a940

**2. [Rule 3 - Blocking] Fixed no_std compilation for async-task**
- **Found during:** Task 2 (kernel build)
- **Issue:** async-task 4.7 defaults to std feature, fails on riscv64gc-unknown-none-elf
- **Fix:** Set `default-features = false` in kernel/Cargo.toml
- **Files modified:** kernel/Cargo.toml
- **Verification:** cargo build succeeds for rv64 target
- **Committed in:** 52988e5

**3. [Rule 3 - Blocking] Fixed extern crate std in no_std test modules**
- **Found during:** Task 1 (host test compilation)
- **Issue:** hal-common is #![no_std] so test modules can't use `std::` paths directly
- **Fix:** Added `extern crate std;` in test modules (test harness links std)
- **Files modified:** hal-common/src/run_queue.rs, timer_wheel.rs, irq_lock.rs
- **Verification:** All 17 host tests compile and pass
- **Committed in:** 313a940

---

**Total deviations:** 3 auto-fixed (1 bug, 2 blocking)
**Impact on plan:** All fixes necessary for correctness and compilation. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviations above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Executor infrastructure complete: spawn, schedule, run queue, timer wheel all working
- SMP boot (Plan 04) can initialize PerCpu for secondary harts and enter executor_loop
- IPI wake (Plan 04) can push to cross-CPU run queues via PerCpu::get(target_cpu)
- Timer wheel tick integration (Plan 04) wires timer IRQ to timer_wheel.advance()

---
*Phase: 01-boot-executor-hal-foundation*
*Completed: 2026-02-23*

## Self-Check: PASSED

All 5 created files verified present. Both task commits (313a940, 52988e5) verified in git log.
