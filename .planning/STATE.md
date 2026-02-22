# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-02-22)

**Core value:** Boot on rv64 QEMU and run real applications without OOM panic or silent memory corruption under SMP
**Current focus:** Phase 2: VM Core + PMAP

## Current Position

Phase: 2 of 7 (VM Core + PMAP)
Plan: 2 of 5 in current phase
Status: In Progress
Last activity: 2026-02-23 -- Completed 02-02-PLAN.md

Progress: [████------] 40% (Phase 2)

## Performance Metrics

**Velocity:**
- Total plans completed: 6
- Average duration: 6min
- Total execution time: 0.58 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 1 | 4/4 | 27min | 7min |
| 2 | 2/5 | 8min | 4min |

**Recent Trend:**
- Last 5 plans: 7min, 6min, 4min, 4min
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

### Pending Todos

None yet.

### Blockers/Concerns

None yet.

## Session Continuity

Last session: 2026-02-23
Stopped at: Completed 02-02-PLAN.md
Resume file: None
