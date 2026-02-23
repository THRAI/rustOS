# Phase 3: Exception Fixup + VFS + Disk + Process - Context

**Gathered:** 2026-02-23
**Status:** Ready for planning

<domain>
## Phase Boundary

Safe user/kernel copy (fixup-gated), filesystem I/O through page cache + lwext4, and fork/exec/wait process lifecycle. The kernel can load and run statically-linked ELF binaries from an ext4 disk image. Hard gate: copy_user fixup tests must pass before any VFS work begins.

</domain>

<decisions>
## Implementation Decisions

### Exception Fixup Mechanism
- BSD-style `pcb_onfault` dynamic state machine: `tp -> PerCpu -> Task.pcb_onfault`
- Hand-written assembly for `copy_user_chunk`, no fixup tables, no PC-range checks
- `sstatus.SUM` bit control: set SUM=1 (bit 18) in prologue before user memory access, clear in both epilogue and landing pad — without SUM, S-mode cannot access U-mode pages
- `copy_user_chunk` is page-aligned atomic: `Chunk_Size = min(N, 4096 - Δ)`, never crosses a page boundary
- Assembly ABI: `extern "C" fn copy_user_chunk(dst, src, len) -> usize` — returns 0 on success, EFAULT on trap via landing pad
- Landing pad clears `pcb_onfault`, sets `a0 = EFAULT`, executes `ret`
- `uiomove` owns POSIX short-read/short-write semantics: swallows EFAULT if `total_copied > 0`, returns `Err(EFAULT)` only if first chunk fails
- Hard gate enforced as separate plan (3a) — dedicated PLAN.md with its own SUMMARY.md, must pass before VFS plans execute
- Hard gate tests: both host unit tests (uiomove page-chunking logic) and QEMU integration tests (real trap path with bad pointers)

### VFS Layer Design
- Minimal trait-based FS interface — no BSD vnode/VOP/namei dispatch table
- Simple iterative path walk: split on `/`, call `.lookup()` on directory vnodes in a loop
- Global `HashMap` dentry cache (decided in drilldown)
- Page cache index: `HashMap<(VnodeId, u64), PageState>` — sufficient for competition
- Two-level file descriptor management: per-process fd table → shared open file table (offset + vnode ref + flags) — fd covers more than file I/O (pipes, sockets later)
- Delegate-only for Phase 3 (both metadata and data through single delegate). Data bypass path (page cache reads raw VirtIO blocks directly) deferred as optimization escape hatch
- Demand-paged file-backed pages: page cache frames mapped read-only for MAP_PRIVATE, COW on write fault
- Boundary page anonymization: when `offset_in_vma + PAGE_SIZE > file_size`, MUST allocate fresh frame, memcpy file portion, zero tail — never mutate page cache frames

### lwext4 Delegation Model
- Single delegate stack-thread, invisible to async executor — blocks on channel, runs lwext4 synchronously, wakes requesting future via Waker
- lwext4 is NOT thread-safe; single delegate serializes all operations — the queue *is* the lock
- Bounded channel (capacity ~256) with async backpressure: `send().await` suspends when full, delegate wakes blocked senders on pop
- `Waker`-based feedback loop: delegate wakes both the result waiter and one blocked sender per completed operation
- Global `blockdev_read`/`blockdev_write` callbacks registered at init, called synchronously from within lwext4
- Adaptive polling + WFI inside blockdev callback (reuse Phase 1 VirtIO pattern): spin first, fall back to WFI if not complete

### ELF Loading + Process Lifecycle
- Demand-paged ELF loading: parse PT_LOAD headers, create VMAs with `vnode + file_offset + file_size`, zero physical allocation at exec time — fault handler does the rest
- VMA struct core fields: `va_start`, `va_end`, `vnode: Option<Arc<dyn Vnode>>`, `file_offset`, `file_size`, `prot`, `flags`
- `file_size` within VMA is the discriminator for fault handler: `offset_in_vma < file_size` → file-backed, else → zero-fill
- Three fault paths: (1) pure file page → map page cache frame read-only, COW on write; (2) boundary page → immediate anonymization (alloc + memcpy + zero tail); (3) pure zero-fill → alloc zeroed frame
- Full COW fork via Phase 2 pmap: clone VMA list, mark all writable pages read-only, new ASID, clone fd table with refcounted open file entries
- `sys_execve` is an async future on the executor: `.await` delegate for ELF header reads, synchronous VMA teardown + rebuild, TrapFrame hijack (sepc = entry point, sp = new stack top, regs zeroed)
- Full `wait4` with WNOHANG + pid filtering, exit status in task struct, parent collects via async `.await`

### COW Refinement: Dual-Layer Tracking
- **Data plane**: per-frame `AtomicU8` refcount array (32KB for 128MB RAM). Trap handler gets O(1) sole-owner truth: `FRAME_REFCOUNTS[PPN].load() == 1` → flip PTE to R|W, zero copy. No VmObject locks in hot path.
- **Control plane**: shadow collapse on `sys_exit`. When a node is sole reference to backing object, hoist pages up and destroy intermediate object. Prevents chain depth explosion (D1→C1→B1→A0 collapses to D1→A0).
- Frame refcount incremented on shared map (fork COW), decremented on unmap/COW copy. Decoupled from VmObject Arc refcount.

### Process Exit + Async Wait Pipeline
- Lost wakeup prevention: inside `WaitChildFuture::poll`, register Waker FIRST (under `IrqSafeSpinLock`), THEN scan children for ZOMBIE. Guarantees no gap between check and suspend.
- Two-phase death: (1) `sys_exit` future completes with `SyscallResult::Terminated` → executor drops coroutine frame (heap freed), removes from run queue, does NOT sret; (2) zombie Task struct lingers (held by parent's `Arc<Task>` in children list) until `wait4` reaps.
- Type-driven executor dispatch via `SyscallResult` enum:
  - `ResumeUserspace(usize)` → write a0, sret to userspace
  - `Terminated` → drop future, remove from run queue, continue to next task
- `sys_exit` signature: `async fn sys_exit(code: i32) -> SyscallResult` (NOT `-> !`)
- Child→parent link is `Weak<Task>` (no circular Arc). Orphans reparented to init (pid 1).
- Memory ordering: `exit_status.store(Release)` + `state.store(ZOMBIE, Release)` in exit; `state.load(Acquire)` in wait4.

### Claude's Discretion
- Exact Rust trait definitions for VFS operations
- Dentry cache eviction policy
- ELF parser implementation details (use existing crate or hand-roll)
- Delegate channel implementation choice (crossbeam-queue vs Mutex<VecDeque>)
- Run queue data structure for executor task management
- Exact adaptive polling spin count in blockdev callback

</decisions>

<specifics>
## Specific Ideas

- copy_user_chunk prologue: explicitly load `p_task` via `ld t0, OFFSET_CURRENT_TASK(tp)` then `sd t1, OFFSET_ONFAULT(t0)` — one extra `ld` instruction to maintain Task context boundary
- Page cache frames are sacred: MAP_PRIVATE mappings never mutate them. Boundary pages always anonymized immediately.
- The bounded delegate channel replaces a filesystem-level global lock: async tasks do O(1) push and suspend, never block on FS locks
- `SyscallResult` enum eliminates runtime state checks in executor — the type system encodes the contract
- Shadow collapse is iterative (not recursive) using `Arc::try_unwrap` — safe for deep fork chains (shell pipelines)

</specifics>

<deferred>
## Deferred Ideas

- VirtIO data bypass path (page cache reads raw blocks directly, bypassing lwext4) — Phase 5+ optimization if physical HW profiling shows I/O bottleneck
- vfork — not needed for Phase 3, full COW fork is sufficient
- Dynamic linking / shared libraries — future phase
- Multiple delegate threads — blocked by lwext4 non-thread-safety
- Radix tree page cache index — only needed if readahead patterns matter on physical HW

</deferred>

---

*Phase: 03-exception-fixup-vfs-disk-process*
*Context gathered: 2026-02-23*
