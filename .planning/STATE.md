# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-02-22)

**Core value:** Boot on rv64 QEMU and run real applications without OOM panic or silent memory corruption under SMP
**Current focus:** Phase 4: Pipes + Signals + Full Syscalls

## Current Position

Phase: 4 of 7 (Pipes + Signals + Full Syscalls) — IN PROGRESS
Plan: 3 of 4 in current phase — 04-03 complete
Status: Real mmap/munmap/mprotect + lseek/fstat/clock_gettime/nanosleep/futex implemented
Last activity: 2026-02-25 -- Plan 04-03 complete (~8min)

Progress: [██████░░░░] 50% (Phase 4)

## Performance Metrics

**Velocity:**
- Total plans completed: 18
- Average duration: 7min
- Total execution time: 1.94 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| 1 | 4/4 | 27min | 7min |
| 2 | 5/5 | 24min | 5min |
| 3 | 5/5 | 51min | 10min |
| 3.1 | 1/1 | 6min | 6min |
| 4 | 2/4 | 16min | 8min |

**Recent Trend:**
- Last 5 plans: 10min, 7min, 6min, 8min, 8min
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

- [03-04]: BTreeMap dentry cache (no eviction, sufficient for competition workloads)
- [03-04]: 3-state page cache (Absent -> Fetching -> Cached) with lock-based CAS transitions
- [03-04]: FdTable uses BTreeMap<u32, Arc<OpenFile>> with next_fd starting at 3
- [03-04]: Boundary page anonymization: fresh frame + memcpy + zero tail, never mutate page cache frames
- [03-04]: Delegate extended with Lookup/Stat/ReadPage for VFS path walk and page cache

- [03-05]: Hand-rolled ELF64 parser over external crate — fewer dependencies, sufficient for static ELF loading
- [03-05]: Demand-paged VMAs: zero physical frame allocation at exec time, fault handler resolves
- [03-05]: Synchronous syscall dispatch for simple syscalls, async escalation for fork/exec/wait4/read/open
- [03-05]: SYS_WRITE to fd 1/2 writes directly to UART (no VFS path for stdout/stderr)

- [03.1-01]: Access faults (5,7) share same match arm as page faults (12,13,15) — pcb_onfault checked first for all
- [03.1-01]: Store access fault mapped to WRITE access type for fault resolution fallthrough

- [04-01]: Three-layer FD model: FdTable(Vec) -> FileDescription(Arc, shared) -> FileObject(enum Vnode|Pipe|Device)
- [04-01]: PipeReadFuture/PipeWriteFuture as explicit Future impls for waker registration
- [04-01]: Console read returns EOF (no input buffer until signal infrastructure)
- [04-01]: ioctl writes directly to user memory via ptr::copy_nonoverlapping (console fds only)
- [04-01]: Stdio pre-populated in new_with_stdio: fd 0=ConsoleRead, fd 1/2=ConsoleWrite

- [04-03]: Top-down mmap allocation below USER_STACK_TOP with MMAP_MIN floor
- [04-03]: MAP_FIXED silently overwrites via remove_range + pmap_remove before insert
- [04-03]: VMA split on partial munmap/mprotect creates fresh VmObjects (demand-paged)
- [04-03]: Futex keyed by physical address via pmap_extract, global BTreeMap<usize, Vec<Waker>>
- [04-03]: clock_gettime reads rdtime CSR, QEMU virt 10MHz timer frequency
- [04-03]: nanosleep delegates to executor timer wheel sleep(ms)

### Pending Todos

None yet.

### Blockers/Concerns

None yet.

## Session Continuity

Last session: 2026-02-25
Stopped at: Plan 04-03 complete, ready for 04-02 (signals) or 04-04 (integration)
Resume file: None
