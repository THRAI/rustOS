# Roadmap: BSD-Async Rust OS Kernel

## Overview

Seven phases take this kernel from first UART output to running git/vim/gcc/rustc on dual architectures. Each phase delivers a vertically complete capability: boot and executor first, then virtual memory, then filesystem and processes, then the remaining syscall surface, then page reclamation and the LA64 port, then networking, and finally real application validation on physical hardware. The hard gate between Phase 3a (exception fixup tests) and the rest of Phase 3 ensures copy_user correctness before building on it.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [ ] **Phase 1: Boot + Executor + HAL Foundation** - Kernel boots on rv64 QEMU with async executor, SMP, trap handling, and debug infrastructure (~3K LOC)
- [ ] **Phase 2: VM Core + PMAP** - Virtual memory subsystem with page tables, ASID management, fault handling, and frame allocation (~6K LOC)
- [ ] **Phase 3: Exception Fixup + VFS + Disk + Process** - User/kernel copy safety, filesystem, block I/O, and fork/exec/wait (~10K LOC, hard gate on fixup tests at 3a)
- [ ] **Phase 4: Pipes + Signals + Full Syscalls** - IPC pipes, signal delivery, and remaining syscall surface for test suite compatibility (~4K LOC)
- [ ] **Phase 5: Page Reclamation + LA64 Port** - Page daemon, writeback daemon, shadow collapse, and full LoongArch64 HAL/PMAP port (~6K LOC)
- [ ] **Phase 6: Network + Sockets** - VirtIO-net, smoltcp integration, TCP sockets, kqueue, and LTP test coverage (~8K LOC)
- [ ] **Phase 7: Apps + Physical Hardware + Tuning** - git, vim, gcc, rustc validation on QEMU and physical board bring-up (~2K LOC)

## Phase Details

### Phase 1: Boot + Executor + HAL Foundation
**Goal**: Kernel boots on rv64 QEMU with a working async executor, SMP, trap infrastructure, and debug tooling -- the foundation everything else builds on
**Depends on**: Nothing (first phase)
**Requirements**: BOOT-01, BOOT-02, BOOT-03, BOOT-04, BOOT-05, BOOT-06, BOOT-07, BOOT-08, BOOT-09, HAL-01, HAL-02, HAL-03, HAL-04, HAL-05, HAL-06, HAL-07, HAL-08, TEST-01, TEST-02
**Success Criteria** (what must be TRUE):
  1. Kernel prints to UART on rv64 QEMU and all secondary harts enter the executor loop
  2. An async future spawned on one CPU can be woken and run on another CPU via IPI without deadlock
  3. Timer IRQ fires at configured interval and a sleeping future wakes after its timeout expires
  4. Trap entry/exit correctly saves and restores all 32 GPRs + CSRs (verified by register-clobber test)
  5. `cargo test --lib` passes for all pure MI logic (locks, timer wheel, run queue) including loom tests for lock-free structures
**Plans**: TBD

Plans:
- [ ] 01-01: TBD
- [ ] 01-02: TBD
- [ ] 01-03: TBD

### Phase 2: VM Core + PMAP
**Goal**: Virtual memory subsystem is complete -- page faults resolve, page tables map correctly, ASIDs avoid TLB aliasing, and frame allocation works under both async and sync contexts
**Depends on**: Phase 1
**Requirements**: VM-01, VM-02, VM-03, VM-04, VM-05, VM-06, VM-07, VM-08, VM-09, VM-11, VM-12, VM-13, VM-14, VM-17, TEST-03
**Success Criteria** (what must be TRUE):
  1. A user process page fault on an anonymous mapping resolves to a new zeroed frame with correct PTE permissions
  2. fork() produces a COW shadow chain and a write fault on either parent or child allocates a private copy without corrupting the other
  3. ASID generation rollover triggers IPI barrier and no stale TLB entries survive (verified by QEMU integration test)
  4. frame_alloc_sync() in trap context returns a frame (or triggers emergency_reclaim_sync) without ever yielding
  5. Iterative VmObject Drop handles a 500-deep shadow chain without stack overflow
**Plans**: TBD

Plans:
- [ ] 02-01: TBD
- [ ] 02-02: TBD
- [ ] 02-03: TBD

### Phase 3: Exception Fixup + VFS + Disk + Process
**Goal**: User/kernel copy is safe (fixup-gated), filesystem reads and writes work through page cache and lwext4, and fork/exec/wait lifecycle is complete -- the kernel can load and run ELF binaries from disk
**Depends on**: Phase 2
**Requirements**: HAL-09, HAL-10, HAL-11, VM-10, VFS-01, VFS-02, VFS-03, VFS-04, VFS-05, VFS-06, VFS-07, VFS-08, PROC-01, PROC-02, PROC-03, PROC-04, PROC-05, TEST-04
**Success Criteria** (what must be TRUE):
  1. [HARD GATE 3a] copy_user_chunk with a bad user pointer returns Fault (not panic), and uiomove crossing a page boundary returns partial done count -- verified by dedicated fixup tests before any VFS work begins
  2. A process can open a file on ext4, read its contents through the page cache, and the second read hits cache (no disk I/O)
  3. fork() + exec() loads an ELF binary from disk and the child runs to exit with correct exit status collected by wait4
  4. The delegate thread runs lwext4 operations without blocking the async executor (verified by concurrent I/O test)
  5. testsuits-for-oskernel basic suite passes at least 80% (~26/33 tests) on rv64 QEMU
**Plans**: TBD

Plans:
- [ ] 03-01: TBD
- [ ] 03-02: TBD
- [ ] 03-03: TBD
- [ ] 03-04: TBD

### Phase 4: Pipes + Signals + Full Syscalls
**Goal**: IPC pipes, POSIX signal delivery, and the remaining syscall surface are complete -- the kernel supports the full interface needed by libc-test and shell pipelines
**Depends on**: Phase 3
**Requirements**: SIG-01, SIG-02, SIG-03, SIG-04, SIG-05, IPC-01, IPC-02, SYS-01, SYS-02, SYS-03, SYS-04, SYS-05, SYS-06
**Success Criteria** (what must be TRUE):
  1. A pipe between two processes transfers data correctly and a reader blocks (parks) when the pipe is empty, waking when the writer writes
  2. sigaction registers a handler, kill delivers the signal, the handler runs in user mode, and sigreturn restores the original execution context
  3. mmap/munmap/mprotect/brk work correctly (verified by a malloc-heavy test that exercises brk growth and mmap fallback)
  4. /dev/null, /dev/zero, /dev/console are functional and ioctl returns plausible terminal info for isatty() checks
  5. futex wait/wake correctly synchronizes two threads (atomic compare + park, wake unblocks waiter)
**Plans**: TBD

Plans:
- [ ] 04-01: TBD
- [ ] 04-02: TBD
- [ ] 04-03: TBD

### Phase 5: Page Reclamation + LA64 Port
**Goal**: The kernel survives memory pressure via page daemon reclamation and shadow collapse, and the full LA64 architecture port boots and passes tests -- dual-arch scoring is unlocked
**Depends on**: Phase 4
**Requirements**: VM-15, VM-16, DMN-01, DMN-02, LA64-01, LA64-02, LA64-03, LA64-04, LA64-05, LA64-06, TEST-05
**Success Criteria** (what must be TRUE):
  1. Under 128MB memory pressure, the page daemon reclaims clean cached pages and the writeback daemon flushes dirty pages -- a memory-hungry workload completes without OOM panic
  2. Shadow collapse via density-aware tree swap runs in bounded time (min(|C|,|P|) insertions) and the collapse daemon yields between 64-page chunks without holding locks across yields
  3. LA64 kernel boots on QEMU to UART output with trap handling, timer IRQ, and SMP functional
  4. LA64 PMAP correctly encodes PTEs with negative permission logic (NR/NX) -- verified by exhaustive unit tests covering all permission combinations
  5. libc-test suite is active on rv64 with meaningful pass rate; LA64 passes the same testsuits-for-oskernel basic suite as rv64
**Plans**: TBD

Plans:
- [ ] 05-01: TBD
- [ ] 05-02: TBD
- [ ] 05-03: TBD

### Phase 6: Network + Sockets
**Goal**: TCP networking works end-to-end -- VirtIO-net driver, smoltcp stack, socket syscalls, and kqueue event notification are functional for git clone over TCP
**Depends on**: Phase 5
**Requirements**: IPC-03, IPC-04, DMN-03, NET-01, NET-02, NET-03, TEST-06
**Success Criteria** (what must be TRUE):
  1. A TCP connection can be established from userspace (connect), data sent/received, and the connection closed cleanly
  2. A server socket can bind, listen, and accept incoming TCP connections
  3. kqueue with EVFILT_READ/EVFILT_WRITE correctly wakes a process waiting on socket readiness
  4. The net_task drives smoltcp's poll loop on a pinned CPU without starving other futures
  5. LTP subset tests are active with meaningful coverage on rv64
**Plans**: TBD

Plans:
- [ ] 06-01: TBD
- [ ] 06-02: TBD
- [ ] 06-03: TBD

### Phase 7: Apps + Physical Hardware + Tuning
**Goal**: Real unmodified applications run on QEMU and the kernel boots on physical hardware -- competition readiness is achieved
**Depends on**: Phase 6
**Requirements**: APP-01, APP-02, APP-03, APP-04, APP-05, TEST-07
**Success Criteria** (what must be TRUE):
  1. git clone over TCP and git push complete successfully on rv64 QEMU
  2. vim starts, displays a file, accepts edits, and handles SIGWINCH (terminal resize) correctly
  3. gcc compiles a hello-world C program under 128MB memory pressure without OOM
  4. rustc compiles a hello-world Rust program on QEMU
  5. Kernel boots on VisionFive 2 (rv64) and 2K1000 (la64) physical boards to UART output and runs basic tests
**Plans**: TBD

Plans:
- [ ] 07-01: TBD
- [ ] 07-02: TBD

## Progress

**Execution Order:**
Phases execute in numeric order: 1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Boot + Executor + HAL Foundation | 0/3 | Not started | - |
| 2. VM Core + PMAP | 0/3 | Not started | - |
| 3. Exception Fixup + VFS + Disk + Process | 0/4 | Not started | - |
| 4. Pipes + Signals + Full Syscalls | 0/3 | Not started | - |
| 5. Page Reclamation + LA64 Port | 0/3 | Not started | - |
| 6. Network + Sockets | 0/3 | Not started | - |
| 7. Apps + Physical Hardware + Tuning | 0/2 | Not started | - |
