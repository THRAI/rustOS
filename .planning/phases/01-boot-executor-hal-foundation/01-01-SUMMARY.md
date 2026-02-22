---
phase: 01-boot-executor-hal-foundation
plan: 01
subsystem: boot, hal
tags: [rv64, qemu, uart, irq, spinlock, no_std, bare-metal]

# Dependency graph
requires:
  - phase: none
    provides: "First plan -- no prior dependencies"
provides:
  - "Bootable rv64 kernel on QEMU virt with UART output"
  - "kprintln! macro for formatted console output"
  - "IRQ enable/disable/save/restore via sstatus.SIE"
  - "hal-common crate: PhysAddr, VirtAddr, Errno, IrqSafeSpinLock"
  - "Cargo workspace (kernel + hal-common)"
  - "Makefile with kernel-rv64, run-rv64, test, clean targets"
affects: [01-02-PLAN, 01-03-PLAN, 01-04-PLAN]

# Tech tracking
tech-stack:
  added: [rust nightly-2025-06-01, qemu-system-riscv64, rust-objcopy]
  patterns: [global_asm boot.S, MMIO UART, IRQ-safe printing, no_std host testing]

key-files:
  created:
    - Cargo.toml
    - rust-toolchain.toml
    - Makefile
    - .cargo/config.toml
    - kernel/Cargo.toml
    - kernel/src/main.rs
    - kernel/src/console.rs
    - kernel/src/hal/mod.rs
    - kernel/src/hal/rv64/mod.rs
    - kernel/src/hal/rv64/boot.S
    - kernel/src/hal/rv64/uart.rs
    - kernel/src/hal/rv64/irq.rs
    - kernel/linker/rv64-qemu.ld
    - hal-common/Cargo.toml
    - hal-common/src/lib.rs
    - hal-common/src/addr.rs
    - hal-common/src/errno.rs
    - hal-common/src/irq_lock.rs
  modified: []

key-decisions:
  - "Used slli instead of mul in boot.S for stack offset (avoids M-extension requirement in asm context)"
  - "Host target auto-detected via rustc -vV for make test portability"
  - "IRQ-safe printing in console.rs uses inline csrrci/csrsi rather than IrqSafeSpinLock to avoid circular dep"

patterns-established:
  - "global_asm! includes boot.S from Rust source tree"
  - "IrqSafeSpinLock with cfg-gated arch_irq for host testability"
  - "kprintln! macro with IRQ masking for safe concurrent output"
  - "Makefile HOST_TARGET detection for cross-compile-safe host tests"

requirements-completed: [BOOT-01, HAL-04, HAL-08, BOOT-09]

# Metrics
duration: 8min
completed: 2026-02-22
---

# Phase 1 Plan 01: Project Scaffold + Boot to UART + hal-common Types Summary

**rv64 kernel boots on QEMU virt printing to UART via kprintln!, with host-testable hal-common providing PhysAddr/VirtAddr/Errno/IrqSafeSpinLock**

## Performance

- **Duration:** 8 min
- **Started:** 2026-02-22T18:57:40Z
- **Completed:** 2026-02-22T19:05:35Z
- **Tasks:** 2
- **Files modified:** 18

## Accomplishments
- Kernel boots on rv64 QEMU and prints "hello world" + formatted output via kprintln!
- IRQ enable/disable/save/restore verified working on real hardware (QEMU)
- hal-common crate compiles on host with 7 passing tests (PhysAddr, VirtAddr, IrqSafeSpinLock)
- Boot stacks allocated for 8 harts, sscratch initialized to 0

## Task Commits

Each task was committed atomically:

1. **Task 1: Cargo workspace + boot.S + linker script + Makefile** - `4886387` (feat)
2. **Task 2: UART driver + kprintln! + IRQ ops + hal-common types** - `5a7f613` (feat)

## Files Created/Modified
- `Cargo.toml` - Workspace root with kernel + hal-common members
- `rust-toolchain.toml` - Pinned nightly-2025-06-01, rv64 + la64 targets
- `Makefile` - Build, run, test, clean targets with host target detection
- `.cargo/config.toml` - Default rv64 target, linker script, frame pointers
- `kernel/Cargo.toml` - Kernel binary crate depending on hal-common
- `kernel/src/main.rs` - rust_main with UART init, kprintln!, IRQ exercise
- `kernel/src/console.rs` - kprint!/kprintln! macros with IRQ-safe output
- `kernel/src/hal/rv64/boot.S` - _start: BSS clear, per-hart stacks, sscratch=0
- `kernel/src/hal/rv64/uart.rs` - UART 16550A driver (init, putchar, getchar)
- `kernel/src/hal/rv64/irq.rs` - IRQ control via sstatus.SIE CSR ops
- `kernel/linker/rv64-qemu.ld` - Memory layout at 0x80200000, 8-hart boot stacks
- `hal-common/src/addr.rs` - PhysAddr/VirtAddr with alignment/arithmetic + 5 tests
- `hal-common/src/errno.rs` - Errno enum with 8 BSD-style error codes
- `hal-common/src/irq_lock.rs` - IrqSafeSpinLock with cfg-gated arch stubs + 2 tests

## Decisions Made
- Used `slli` (shift) instead of `mul` in boot.S for stack offset calculation -- avoids M-extension requirement in raw assembly context
- Host target auto-detected via `rustc -vV` in Makefile for portable `make test`
- Console printing uses inline IRQ masking rather than IrqSafeSpinLock to avoid circular dependency between kernel and hal-common

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed cargo config.toml runner table syntax**
- **Found during:** Task 1 (build verification)
- **Issue:** `[target.riscv64gc-unknown-none-elf.runner]` was a TOML table header but cargo expects a string
- **Fix:** Changed to a comment
- **Files modified:** .cargo/config.toml
- **Verification:** cargo build succeeds
- **Committed in:** 4886387

**2. [Rule 1 - Bug] Replaced mul with slli in boot.S**
- **Found during:** Task 1 (build verification)
- **Issue:** `mul` instruction not available in base ISA assembly context (requires Zmmul)
- **Fix:** Used `slli t1, a0, 14` since STACK_SIZE=16384 is 2^14
- **Files modified:** kernel/src/hal/rv64/boot.S
- **Verification:** cargo build succeeds, QEMU boots correctly
- **Committed in:** 4886387

**3. [Rule 3 - Blocking] Fixed hal-common test target**
- **Found during:** Task 1 (test verification)
- **Issue:** `cargo test` used default rv64 target from .cargo/config.toml, failing with missing test crate
- **Fix:** Added HOST_TARGET detection in Makefile, pass `--target $(HOST_TARGET)` to test command
- **Files modified:** Makefile
- **Verification:** `make test` passes on host
- **Committed in:** 4886387

---

**Total deviations:** 3 auto-fixed (2 bugs, 1 blocking)
**Impact on plan:** All fixes necessary for correct build and test. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviations above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Boot infrastructure complete: kernel boots, prints, IRQ ops work
- hal-common foundation ready for timer wheel and executor types (Plan 03)
- Trap entry/exit assembly (Plan 02) can build on this boot.S and linker script
- kprintln! available for debug output in all subsequent plans

---
*Phase: 01-boot-executor-hal-foundation*
*Completed: 2026-02-22*

## Self-Check: PASSED

All 18 created files verified present. Both task commits (4886387, 5a7f613) verified in git log.
