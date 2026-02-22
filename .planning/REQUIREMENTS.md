# Requirements: BSD-Async Rust OS Kernel

**Defined:** 2026-02-22
**Core Value:** Boot on rv64 QEMU and run real applications without OOM panic or silent memory corruption under SMP

## v1 Requirements

Requirements for competition submission. Each maps to roadmap phases per BLACKBOX.md Section 11b.

### Boot & Executor

- [x] **BOOT-01**: Kernel boots on rv64 QEMU to UART output
- [x] **BOOT-02**: Per-CPU async executor polls futures from run queues
- [x] **BOOT-03**: Run queues protected by IrqSafeSpinLock (type-enforced IRQ masking)
- [x] **BOOT-04**: async-task crate integration (spawn, Runnable, schedule_fn callback)
- [x] **BOOT-05**: Timer wheel supports sleep/timeout futures
- [x] **BOOT-06**: Timer IRQ fires and triggers cooperative preemption via yield_now
- [x] **BOOT-07**: IPI infrastructure (SBI send_ipi on rv64) for cross-CPU wake
- [x] **BOOT-08**: SMP boot (hart_start via SBI, secondary CPUs enter executor)
- [x] **BOOT-09**: Debug infrastructure: kprintln! macro, GDB stub support, QEMU trace flags

### HAL (Machine-Dependent)

- [x] **HAL-01**: Trap entry/exit assembly saves/restores 32 GPRs + CSRs
- [x] **HAL-02**: Separate user and kernel trap vectors (stvec switching)
- [x] **HAL-03**: Trap dispatch classifies cause (syscall, page fault, timer IRQ, external IRQ)
- [x] **HAL-04**: UART driver for console I/O
- [x] **HAL-05**: Timer setup (set_oneshot for next timer IRQ)
- [x] **HAL-06**: TLB flush operations (flush_addr, flush_asid, flush_all)
- [x] **HAL-07**: Cache ops (dcache_wb, dcache_inv) for DMA correctness
- [x] **HAL-08**: IRQ enable/disable/scoped_disable
- [ ] **HAL-09**: Exception fixup: pcb_onfault field on task struct, O(1) check in trap handler
- [ ] **HAL-10**: #[naked] copy_user_chunk wrapping C memcpy with fixup landing pad (rv64)
- [ ] **HAL-11**: uiomove: page-at-a-time chunked copy with explicit done counter, trinary return (Ok/Fault/NeedsIO)

### VM & PMAP

- [x] **VM-01**: VmMap using BTreeMap with sorted VmAreas
- [x] **VM-02**: VmArea with monotonic AtomicU64 ID for TOCTOU defense
- [x] **VM-03**: VmObject with shadow chain via Arc<RwLock<VmObject>>
- [x] **VM-04**: Iterative Drop via Arc::try_unwrap loop (no stack overflow on 500+ deep chains)
- [x] **VM-05**: PageOwnership enum distinguishes Anonymous (freed on drop) from Cached (page cache manages)
- [x] **VM-06**: Frame allocator: buddy system + per-CPU magazine
- [x] **VM-07**: Dual frame_alloc API: frame_alloc() async (can yield) + frame_alloc_sync() (trap context, never yields)
- [x] **VM-08**: emergency_reclaim_sync() scans inactive queue for clean pages without yielding
- [ ] **VM-09**: sync_fault_handler resolves anonymous and cached-page faults on trap stack
- [ ] **VM-10**: fault_in_page().await fetches file-backed pages via page cache asynchronously
- [ ] **VM-11**: Shared page table walk parameterized by const LEVELS (3 for Sv39)
- [ ] **VM-12**: pmap_enter, pmap_remove, pmap_protect, pmap_extract
- [ ] **VM-13**: ASID allocator with generation-based rollover + synchronous IPI barrier
- [ ] **VM-14**: pmap_shootdown via IPI + per-CPU ack
- [ ] **VM-15**: Density-aware tree swap for shadow collapse (min(|C|,|P|) insertions)
- [ ] **VM-16**: Background collapse daemon: chunked (64 pages), bounded lock hold, yield between chunks
- [x] **VM-17**: Memory poisoning (debug builds) and trap stack canaries

### VFS & Disk

- [ ] **VFS-01**: Vnode enum dispatch (Ext4 | Tmpfs | Devfs | Pipe | Socket)
- [ ] **VFS-02**: FdTable per-process (fd number → FileDesc)
- [ ] **VFS-03**: Page cache with 3-state CAS (absent → Fetching(WakerList) → Cached(Frame))
- [ ] **VFS-04**: Dentry cache (parent_vnode, name → vnode)
- [ ] **VFS-05**: sys_open, sys_read, sys_write, sys_close, sys_stat, sys_readdir
- [ ] **VFS-06**: Delegate thread runs lwext4 synchronously without blocking executor
- [ ] **VFS-07**: lwext4 FFI with global blockdev callback for VirtIO adaptive polling
- [ ] **VFS-08**: VirtIO-blk driver (MMIO transport on rv64)

### Process

- [ ] **PROC-01**: fork via COW (vm_object_shadow inserts shadow at chain head)
- [ ] **PROC-02**: exec loads ELF, resets vm_map
- [ ] **PROC-03**: wait4 and exit with parent/child lifecycle
- [ ] **PROC-04**: getpid, getppid
- [ ] **PROC-05**: Persistent UserTaskFuture wrapping run_tasks loop (no per-syscall spawn)

### Signal

- [ ] **SIG-01**: sigaction registers signal handlers
- [ ] **SIG-02**: sigprocmask manages blocked signal set
- [ ] **SIG-03**: kill posts signal to target process
- [ ] **SIG-04**: check_pending_signals at userret modifies trap frame for delivery
- [ ] **SIG-05**: sigreturn restores original frame after signal handler

### IPC

- [ ] **IPC-01**: Pipe with ring buffer + waker per end
- [ ] **IPC-02**: Futex wait (atomic compare + park) and wake
- [ ] **IPC-03**: TCP sockets wrapping smoltcp (connect, recv, send, accept)
- [ ] **IPC-04**: kqueue with EVFILT_READ, EVFILT_WRITE (waker slot evolves to WakerList)

### Daemons

- [ ] **DMN-01**: Page daemon as long-lived async future (clock scan inactive queue)
- [ ] **DMN-02**: Writeback daemon flushes dirty pages via delegate, marks clean
- [ ] **DMN-03**: Net task drives smoltcp poll loop, pinned to 1 CPU

### Network

- [ ] **NET-01**: VirtIO-net driver (MMIO on rv64, PCI on la64)
- [ ] **NET-02**: smoltcp integration (TCP, UDP, DNS)
- [ ] **NET-03**: Socket syscalls (socket, bind, listen, accept, connect, send, recv)

### LA64 Parallel Track

- [ ] **LA64-01**: LA64 HAL: boot.S, trap entry/exit (ertn), UART, timer
- [ ] **LA64-02**: LA64 PMAP: 4-level page table walk, pgdl/pgdh split
- [ ] **LA64-03**: LA64 PTE encoding with negative permission logic (NR/NX) + exhaustive unit tests
- [ ] **LA64-04**: LA64 ASID allocator (10-bit, 1024 ASIDs, frequent rollover + IPI barrier)
- [ ] **LA64-05**: LA64 copy_user_chunk #[naked] asm (mechanical translation of rv64)
- [ ] **LA64-06**: LA64 interrupt controller (HWI vs PLIC)

### Syscalls (Remaining)

- [ ] **SYS-01**: dup, dup2, dup3
- [ ] **SYS-02**: lseek, fstat
- [ ] **SYS-03**: mmap, munmap, mprotect, brk
- [ ] **SYS-04**: clock_gettime, nanosleep
- [ ] **SYS-05**: ioctl (terminal basics)
- [ ] **SYS-06**: /dev/null, /dev/zero, /dev/console (devfs)

### Testing

- [x] **TEST-01**: Tier 1 host unit tests (cargo test --lib) for pure MI logic
- [x] **TEST-02**: Tier 1.5 host loom tests for lock-free structures and CAS state machines
- [ ] **TEST-03**: Tier 2 QEMU kernel integration tests (custom_test_frameworks)
- [ ] **TEST-04**: Tier 3a testsuits-for-oskernel basic (~33 tests) pass >=80%
- [ ] **TEST-05**: Tier 3b libc-test (~463 tests) active
- [ ] **TEST-06**: Tier 3c LTP subset (~366 categories) active
- [ ] **TEST-07**: App tests: git, vim, gcc, rustc run on QEMU

### Apps & Hardware

- [ ] **APP-01**: git clone/push works over TCP
- [ ] **APP-02**: vim runs with terminal ioctl and SIGWINCH
- [ ] **APP-03**: gcc compiles hello world under 128MB memory pressure
- [ ] **APP-04**: rustc compiles hello world
- [ ] **APP-05**: Physical hardware boot (VisionFive 2 rv64, 2K1000 la64)

## v2 Requirements

Deferred post-competition.

- **V2-01**: VirtIO bypass for page cache data reads (eliminate queue depth 1 bottleneck)
- **V2-02**: BSD UMA Zone<T> allocator (per-object-type magazines)
- **V2-03**: Swap daemon (anonymous page eviction to disk)
- **V2-04**: Full LTP test coverage beyond competition subset
- **V2-05**: Performance benchmarks (iozone, lmbench)

## Out of Scope

| Feature | Reason |
|---------|--------|
| Real-time scheduling | Cooperative preemption at trap boundaries sufficient for competition |
| GPU/display drivers | Competition is terminal-only |
| Filesystem other than ext4 | lwext4 proven by both competitors |
| 32-bit architecture support | Competition is 64-bit only |
| Userspace dynamic linker | musl static linking sufficient for competition apps |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| BOOT-01 | Phase 1 | Complete |
| BOOT-02 | Phase 1 | Complete |
| BOOT-03 | Phase 1 | Complete |
| BOOT-04 | Phase 1 | Complete |
| BOOT-05 | Phase 1 | Complete |
| BOOT-06 | Phase 1 | Complete |
| BOOT-07 | Phase 1 | Complete |
| BOOT-08 | Phase 1 | Complete |
| BOOT-09 | Phase 1 | Complete |
| HAL-01 | Phase 1 | Complete |
| HAL-02 | Phase 1 | Complete |
| HAL-03 | Phase 1 | Complete |
| HAL-04 | Phase 1 | Complete |
| HAL-05 | Phase 1 | Complete |
| HAL-06 | Phase 1 | Complete |
| HAL-07 | Phase 1 | Complete |
| HAL-08 | Phase 1 | Complete |
| HAL-09 | Phase 3 | Pending |
| HAL-10 | Phase 3 | Pending |
| HAL-11 | Phase 3 | Pending |
| VM-01 | Phase 2 | Complete |
| VM-02 | Phase 2 | Complete |
| VM-03 | Phase 2 | Complete |
| VM-04 | Phase 2 | Complete |
| VM-05 | Phase 2 | Complete |
| VM-06 | Phase 2 | Complete |
| VM-07 | Phase 2 | Complete |
| VM-08 | Phase 2 | Complete |
| VM-09 | Phase 2 | Pending |
| VM-10 | Phase 3 | Pending |
| VM-11 | Phase 2 | Pending |
| VM-12 | Phase 2 | Pending |
| VM-13 | Phase 2 | Pending |
| VM-14 | Phase 2 | Pending |
| VM-15 | Phase 5 | Pending |
| VM-16 | Phase 5 | Pending |
| VM-17 | Phase 2 | Complete |
| VFS-01 | Phase 3 | Pending |
| VFS-02 | Phase 3 | Pending |
| VFS-03 | Phase 3 | Pending |
| VFS-04 | Phase 3 | Pending |
| VFS-05 | Phase 3 | Pending |
| VFS-06 | Phase 3 | Pending |
| VFS-07 | Phase 3 | Pending |
| VFS-08 | Phase 3 | Pending |
| PROC-01 | Phase 3 | Pending |
| PROC-02 | Phase 3 | Pending |
| PROC-03 | Phase 3 | Pending |
| PROC-04 | Phase 3 | Pending |
| PROC-05 | Phase 3 | Pending |
| SIG-01 | Phase 4 | Pending |
| SIG-02 | Phase 4 | Pending |
| SIG-03 | Phase 4 | Pending |
| SIG-04 | Phase 4 | Pending |
| SIG-05 | Phase 4 | Pending |
| IPC-01 | Phase 4 | Pending |
| IPC-02 | Phase 4 | Pending |
| IPC-03 | Phase 6 | Pending |
| IPC-04 | Phase 6 | Pending |
| DMN-01 | Phase 5 | Pending |
| DMN-02 | Phase 5 | Pending |
| DMN-03 | Phase 6 | Pending |
| NET-01 | Phase 6 | Pending |
| NET-02 | Phase 6 | Pending |
| NET-03 | Phase 6 | Pending |
| LA64-01 | Phase 5 | Pending |
| LA64-02 | Phase 5 | Pending |
| LA64-03 | Phase 5 | Pending |
| LA64-04 | Phase 5 | Pending |
| LA64-05 | Phase 5 | Pending |
| LA64-06 | Phase 5 | Pending |
| SYS-01 | Phase 4 | Pending |
| SYS-02 | Phase 4 | Pending |
| SYS-03 | Phase 4 | Pending |
| SYS-04 | Phase 4 | Pending |
| SYS-05 | Phase 4 | Pending |
| SYS-06 | Phase 4 | Pending |
| TEST-01 | Phase 1 | Complete |
| TEST-02 | Phase 1 | Complete |
| TEST-03 | Phase 2 | Pending |
| TEST-04 | Phase 3 | Pending |
| TEST-05 | Phase 5 | Pending |
| TEST-06 | Phase 6 | Pending |
| TEST-07 | Phase 7 | Pending |
| APP-01 | Phase 7 | Pending |
| APP-02 | Phase 7 | Pending |
| APP-03 | Phase 7 | Pending |
| APP-04 | Phase 7 | Pending |
| APP-05 | Phase 7 | Pending |

**Coverage:**
- v1 requirements: 89 total
- Mapped to phases: 89
- Unmapped: 0

---
*Requirements defined: 2026-02-22*
*Last updated: 2026-02-22 after roadmap creation*
