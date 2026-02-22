# BSD-Async Rust OS Kernel

## What This Is

A BSD-architecture async Rust OS kernel for the 2025 OS National Competition. Targets rv64 and la64 (QEMU + physical hardware), running real unmodified Linux applications — git, vim, gcc, rustc — under memory pressure. Built from scratch by a 2-person team, with BSD as the architectural reference and chronix/delonix as practical feasibility baselines.

## Core Value

The kernel must boot on rv64 QEMU and run real applications without OOM panic or silent memory corruption under SMP — correctness under pressure is the non-negotiable.

## Requirements

### Validated

(None yet — ship to validate)

### Active

- [ ] Boot on rv64 QEMU with async executor running futures
- [ ] Per-CPU run queues with IrqSafeSpinLock (no IRQ-reentrance deadlock)
- [ ] HAL: trap entry/exit, timer IRQ, UART console output
- [ ] Persistent task future model (UserTaskFuture wrapping run_tasks loop)
- [ ] Preemption via timer IRQ + yield_now at trap boundary
- [ ] IPI infrastructure for cross-CPU wake and future TLB shootdown
- [ ] VM core: VmMap, VmObject with shadow chain, iterative Drop
- [ ] PMAP: page table walk (Sv39), pmap_enter/remove, ASID allocator with generation rollover IPI barrier
- [ ] Exception fixup: pcb_onfault, #[naked] copy_user_chunk, uiomove trinary return
- [ ] VFS: page cache (3-state CAS), fd table, vnode dispatch
- [ ] Delegate thread: lwext4 FFI with adaptive VirtIO polling
- [ ] Process: fork (COW via shadow chain), exec (ELF load), wait/exit
- [ ] IPC: pipes, futex
- [ ] Signal: sigaction, delivery at userret, sigreturn
- [ ] Page daemon + writeback daemon as long-lived futures
- [ ] Dual frame allocator API: frame_alloc() async + frame_alloc_sync()
- [ ] Network: smoltcp via net_task, TCP sockets, kqueue
- [ ] Dual-arch: LA64 HAL, LA64 PMAP (4-level, pgdl/pgdh, negative PTE logic), LA64 copy_user_chunk
- [ ] Pass ≥90% automated test suite on both rv64 and la64
- [ ] Run git, vim, gcc, rustc on QEMU without OOM or corruption

### Out of Scope

- Real-time scheduling — cooperative preemption at trap boundaries is sufficient for competition
- Swap daemon — competition workloads are file-backed page dominated; anonymous pressure is secondary
- BSD UMA Zone allocator — buddy_system_allocator + GlobalAlloc is sufficient (same as competitors)
- VirtIO bypass for data reads — acceptable queue depth 1 on QEMU; escape hatch for physical hardware only
- Mobile/embedded targets — QEMU + VisionFive 2 + 2K1000 only

## Context

- **Competition**: 2025 OS National Competition. Prelim: 25% rv64 + 25% la64 automated tests + 50% design doc. On-site: 55% git + 15% vim + 15% gcc + 15% rustc + 5% design.
- **Team**: 2 people. One owns rv64 main track, one owns LA64 parallel track. MI code is shared.
- **Reference kernels**: chronix (~36K LOC, first prize 2024) and delonix (~36K LOC, first prize 2024) — both pass basic suite, both have gaps in exception fixup, page reclamation, and TLB shootdown.
- **Architecture reference**: FreeBSD source (trap.c, vm_object.c, pmap.c) for BSD-correct implementations.
- **Concept documents**: Detailed architecture in `concept_docs/BLACKBOX.md`, drill-down analysis in `concept_docs/DRILLDOWN.md`, full concept in `concept_docs/CONCEPT-DOC.md`.
- **Existing code in repo**: chronix and delonix submodules for reference; freebsd-src for BSD source review; testsuits submodule for competition test cases.
- **Timeline**: 3-4 months to prelim deadline.

## Constraints

- **Arch**: rv64 (RISC-V 64, Sv39) and la64 (LoongArch64, 4-level page tables) — both required for full scoring
- **Toolchain**: Rust nightly (pinned), no_std, no_main, vendored dependencies (offline build required)
- **Hardware**: QEMU virt machine (primary), VisionFive 2 (rv64 physical), 2K1000 (la64 physical)
- **Memory**: 128-256MB QEMU — must survive GCC link phase under this constraint
- **Filesystem**: lwext4 (C FFI) via delegate thread — proven by both competitors
- **Network**: smoltcp (Rust, no_std) — needs TAP prototype validation before Phase 6
- **Test suites**: testsuits-for-oskernel basic (~33), libc-test (~463), LTP subset (~366 categories)

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| BSD pcb_onfault over Linux .fixup table | One copy primitive (uiomove), O(1) trap handler check, proven BSD mechanism | — Pending |
| IrqSafeSpinLock for run queues | Prevents IRQ-reentrance deadlock that competitors leave latent | — Pending |
| Persistent task future (no per-syscall spawn) | Proven by both competitors, fast syscalls complete in one poll | — Pending |
| Dual frame_alloc API (async + sync) | Trap context cannot yield; self-throttling reclamation requires both paths | — Pending |
| Density-aware tree swap for shadow collapse | min(|C|,|P|) insertions instead of max; near-zero work in common case | — Pending |
| From scratch (not forking competitors) | Architectural advantages require co-designed sync/async boundary from the start | — Pending |
| BSD for architecture, competitors for feasibility | BSD is correct; competitors prove what's practical in competition context | — Pending |

---
*Last updated: 2026-02-22 after initialization*
