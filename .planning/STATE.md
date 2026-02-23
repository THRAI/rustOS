# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-02-22)

**Core value:** Boot on rv64 QEMU and run real applications without OOM panic or silent memory corruption under SMP
**Current focus:** Phase 3: Exception Fixup + VFS + Disk + Process

## Current Position

Phase: 3 of 7 (Exception Fixup + VFS + Disk + Process)
Plan: 4 of 5 in current phase
Status: Executing
Last activity: 2026-02-23 -- Plan 03-02 complete (VirtIO-blk + ext4 + delegate)

Progress: [████████░░] 80% (Phase 3)

## Performance Metrics

**Velocity:**
- Total plans completed: 13
- Average duration: 6min
- Total execution time: 1.30 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 1 | 4/4 | 27min | 7min |
| 2 | 5/5 | 24min | 5min |
| 3 | 4/5 | 34min | 9min |

**Recent Trend:**
- Last 5 plans: 8min, 10min, 10min, 6min, 8min
- Trend: stable

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table.
Recent decisions affecting current work:

- [Init]: BSD pcb_onfault over Linux .fixup table for exception fixup
- [Init]: IrqSafeSpinLock for run queues (prevents IRQ-reentrance deadlock)
- [Init]: Persistent task future model (no per-syscall spawn)
- [Init]: Dual frame_alloc API (async + sync) for trap context safety
- [Init]: From scratch build (not forking competitors)
- [01-01]: slli over mul in boot.S (avoids M-extension in asm context)
- [01-01]: Host target auto-detection in Makefile for portable testing
- [01-01]: Inline IRQ masking in console.rs (avoids circular dep with hal-common)
- [01-02]: SBI Timer extension (EID 0x54494D45) over legacy ecall for set_timer
- [01-02]: 37-field TrapFrame (296 bytes) with compile-time size assert
- [01-02]: Bump allocator for early boot (unblocks hal-common alloc dep)
- [01-03]: async-task with default-features=false for no_std kernel target
- [01-03]: spin::Once for PerCpu array (bypasses const-init limits of VecDeque/Vec)
- [01-03]: Minimum 1 tick delay in timer wheel insert (prevents slot-0 miss)
- [01-04]: cpu_id-indexed boot stacks for secondary harts (mhartid unavailable in S-mode)
- [01-04]: Hand-parsed FDT /cpus node (no external crate dependency)
- [01-04]: needs_reschedule AtomicBool on PerCpu for cooperative preemption
- [02-01]: core::mem::take over BTreeMap::drain (drain unstable in no_std nightly)
- [02-01]: bitflags crate for MapPerm instead of manual bit manipulation
- [02-01]: Adapted os/src/mm/vm/ paths to kernel/src/mm/vm/ (project uses kernel/ not os/)
- [02-02]: PhysAddr-based API (not PhysPageNum) — matches existing hal-common addr types
- [02-02]: Static magazine array indexed by cpu_id (avoids modifying PerCpu struct)
- [02-02]: IrqSafeSpinLock wrapping per-CPU magazines for IRQ safety
- [02-03]: PteFlags bit positions mirror Sv39 hardware for zero-cost conversion
- [02-03]: Leaked FrameTracker for PT pages (freed on pmap teardown, not RAII per-page)
- [02-03]: Walk returns raw *mut PteRaw pointer + level for max caller flexibility
- [02-05]: std::sync::Mutex guard for ASID tests sharing mutable global state (parallel safety)
- [02-05]: QEMU iterative drop reduced to 200-deep (host covers 500+/1000+, QEMU 15s timeout)
- [02-05]: VM integration tests at 400ms delay to avoid satp-switch timing conflict
- [03-01]: global_asm! with const offset_of!(PerCpu, pcb_onfault) — eliminates offset mismatch bugs
- [03-01]: Trap handler covers access faults (5,7) + page faults (13,15) — QEMU raises access faults for unmapped addresses
- [03-01]: Pluggable copy_fn in kernel-mm uio for host testability
- [03-03]: Weak<Task> for child-to-parent link (no circular Arc)
- [03-03]: AtomicU8 TaskState + AtomicI32 exit_status with Release/Acquire ordering
- [03-03]: WaitChildFuture registers Waker BEFORE scanning children (no lost wakeup)
- [03-03]: Linux-compatible syscall numbers for rv64 (SYS_EXIT=93, SYS_GETPID=172, SYS_GETPPID=173)

- [03-02]: Pure-Rust ext4 parser instead of lwext4 FFI — avoids C dependency, sufficient for read-only boot path
- [03-02]: Delegate as async task (not OS thread) — reuses executor, yields cooperatively
- [03-02]: ReplySlot with AtomicBool + IrqSafeSpinLock for oneshot wakeup — no alloc per request
- [03-02]: Python ext4 image builder for macOS compatibility (no mkfs.ext4 needed)

### Pending Todos

None yet.

### Blockers/Concerns

None yet.

## Session Continuity

Last session: 2026-02-23
Stopped at: Plan 03-02 complete (VirtIO-blk + ext4 + delegate), ready for plan 03-04
Resume file: None
