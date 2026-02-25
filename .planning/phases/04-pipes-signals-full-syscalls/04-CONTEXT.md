# Phase 4: Pipes + Signals + Full Syscalls - Context

**Gathered:** 2026-02-25
**Status:** Ready for planning

<domain>
## Phase Boundary

IPC pipes, POSIX signal delivery, device nodes (/dev/null, /dev/zero, /dev/console), and the remaining syscall surface (mmap, munmap, mprotect, brk, dup, lseek, fstat, clock_gettime, nanosleep, ioctl, futex). The kernel supports the full interface needed by musl libc-test and busybox shell pipelines. This is the "make userspace actually work" phase.

</domain>

<decisions>
## Implementation Decisions

### Signal Delivery Model
- **Sigcode page**: Dedicated read-only page mapped into every process at a fixed VA. Contains the `sigreturn` trampoline (`ecall SYS_rt_sigreturn`). Signal frame on user stack points `ra` here. Matches FreeBSD `sendsig()`.
- **Core 14 signals**: SIGKILL, SIGTERM, SIGCHLD, SIGPIPE, SIGSEGV, SIGBUS, SIGFPE, SIGILL, SIGINT, SIGSTOP, SIGCONT, SIGALRM, SIGUSR1, SIGUSR2. Covers shell job control, pipe EOF, fault reporting, timers.
- **sigaction flags**: SA_SIGINFO (full siginfo_t with sender pid, fault address), SA_RESTART (auto-restart interrupted syscalls), SA_NOCLDWAIT (SIGCHLD).
- **Per-task signal mask**: Signal mask (pending + blocked sets) stored directly in Task struct. `check_pending_signals` at userret ANDs pending with ~blocked.
- **sendsig with pcb_onfault guard**: Build sigframe in kernel memory, `copyout` to user stack. If user SP is trashed, copyout fails via pcb_onfault → process killed with SIGILL. No double-fault risk. Verified against FreeBSD `sys/riscv/riscv/exec_machdep.c:sendsig()`.
- **sigaltstack**: Supported. Task struct holds `ss_sp + ss_size + ss_flags`. When SA_ONSTACK is set and SIGSEGV fires, kernel ignores corrupted SP and uses altstack as safe haven for sigframe construction.
- **sigreturn sanitization (three disciplines)**:
  1. **Privilege masking**: `sanitized_sstatus = (user_sstatus & !SSTATUS_SPP) | SSTATUS_SPIE` — force User mode, force interrupts enabled.
  2. **PC bounds check**: `sepc < USER_MAX_VA` — reject kernel-space return addresses with SIGSEGV.
  3. **CSR immunity**: Only restore GPRs (a0-a7, t0-t6, s0-s11) and sanitized sstatus/sepc from ucontext. Never restore satp, sscratch, or any privileged CSR from user-provided data.

### Syscall Restart & EINTR Policy
- **Two equivalence classes**: Blocking syscalls split into idempotent state-driven (restartable) vs time/sync-driven (always EINTR).
- **Restart whitelist** (SA_RESTART → `sepc -= 4`): read, write, wait4. These are idempotent — retrying doesn't corrupt state.
- **Always EINTR**: nanosleep (must write remaining time to `rem` pointer — time is irreversible), futex_wait (user-space atomic may have mutated during handler — must re-evaluate CAS).
- **Restart mechanism**: Syscall dispatcher checks if signal was just delivered + SA_RESTART set + syscall in whitelist → `trap_frame.sepc -= 4` (rewind to ecall instruction), clear return value. On sret, CPU re-executes ecall as if freshly issued by userspace.
- **Non-restart path**: Preserve `a0 = -EINTR`, return to userspace.

### Async Signal Injection (Cooperative Cancellation)
- **Injection (O(1))**: `sys_kill` does `target.pending_signals.fetch_or(signal_bit)` + `target.top_level_waker.wake()`. Two atomic ops, no lock.
- **Top-level Waker**: Task struct holds a persistent waker that always re-enqueues to runqueue, orthogonal to whatever subsystem wait queue (pipe, timer, futex) holds a copy.
- **Guard clause**: Every blocking `poll` method checks `current_task().has_unmasked_pending_signals()` at entry. If set → `Poll::Ready(Err(EINTR))`. No destructive cancellation — task wakes, re-enters poll, hits guard, exits cleanly.

### Process Groups (Minimal pgrp)
- **pgid field**: `pgid: u32` in Task struct. Inherited from parent on fork.
- **Syscalls**: `setpgid(pid, pgid)` and `getpgid(pid)`. busybox sh uses these to group pipeline members.
- **Broadcast kill**: `kill(-pgrp, sig)` iterates global task list, applies cooperative signal injection to all tasks matching pgid.
- **No sessions**: No `setsid`, no `tcsetpgrp`, no SIGTTIN/SIGTTOU, no controlling terminal concept. Full job control deferred.

### wait4 / Zombie Lifecycle
- **Parent waiter_waker**: Stored in parent's Task struct. `sys_wait4` registers waker and returns `Poll::Pending`.
- **Child exit sequence**: Close all FDs (triggers pipe death) → clear VmMap (release physical memory) → zombie state (retain Task struct + exit code) → wake parent's `waiter_waker`.
- **Reap**: Parent wakes from wait4, finds zombie in child list, extracts exit code, drops Task struct completely.

### Pipe & Futex Design
- **Fixed 4KB ring buffer**: No dynamic resizing. Writer returns `Poll::Pending` when full, reader returns `Poll::Pending` when empty. `IrqSafeSpinLock` protects `PipeInner`.
- **Waker storage**: `Option<Waker>` per end (not `Vec<Waker>`). Unconditional overwrite on each poll — zero leak, zero `will_wake` scan. Sufficient for 1:1 shell pipe topology. **Tech debt**: M:N multi-reader pipes need upgrade to dedup wait queue in future phase.
- **Wake ordering**: Register waker WHILE holding lock (atomicity with emptiness/fullness check). Wake AFTER releasing lock (avoid thundering herd under lock). Prevents lost wakeups.
- **PIPE_BUF atomic write**: Writes ≤ PIPE_BUF (4096): if `free < data.len()`, return `Poll::Pending` without writing anything — all-or-nothing. Writes > PIPE_BUF: partial writes allowed. Verified against FreeBSD `sys/kern/sys_pipe.c:1261-1263`.
- **SIGPIPE death detection**: Reader Drop sets `reader_closed = true` + wakes all sleeping writers. Writer poll sees `reader_closed` → returns `EPIPE` → syscall layer posts SIGPIPE to calling task → delivered at userret.
- **Futex**: Process-private only. Keyed by **physical address** (walk page table on trap entry to get PA). Global `HashMap<PhysAddr, Vec<Waker>>`. FUTEX_WAIT: atomic compare + park. FUTEX_WAKE: wake N waiters.

### mmap / brk Behavior
- **MAP_FIXED**: Silently overwrite existing mappings in the range (BSD behavior — `vm_map_delete` before insert).
- **Address space layout (BSD)**: Stack at top of user VA, mmap region allocated top-down (searching downward from below stack), brk grows upward from .bss end. Maximum separation between zones.
- **brk region**: Single accordion VmArea extending from page-aligned .bss end. `brk_current` pointer slides monotonically within this region. In-place expand/shrink as designed in Phase 3.
- **mmap region**: Fragmented playground of discrete VMAs with varying permissions and backing. `find_free_area` searches downward from mmap base, never crosses into brk growth path.
- **mmap scope**: Anonymous (MAP_ANONYMOUS + MAP_PRIVATE) and private file-backed (MAP_PRIVATE with file). No MAP_SHARED — avoids writeback state machine entirely.
  - **Anonymous**: Zero-page on demand. Satisfies musl malloc.
  - **Private file-backed**: Read → page cache RO mapping. Write fault → COW to anonymous page (allocate new frame, copy file data, map R|W). Modifications never pollute page cache, no disk writeback. Reuses Phase 3's `fault_in_page` logic.

### FetchGuard Abort Safety (Page Cache)
- **RAII FetchGuard**: Wraps the alpha wolf's I/O fetch. Holds reference to the CacheEntry in Fetching state.
- **Drop rollback**: On unexpected drop (SIGKILL, timeout), Guard's `Drop` impl rolls back `Fetching → Absent`, extracts and wakes all queued wakers. Next woken thread re-claims alpha wolf role and re-initiates I/O from scratch.
- **Zero-fill tail**: Mandatory `memset(0)` of `(PAGE_SIZE - bytes_read)` on partial-page file reads (e.g., 5000-byte file, second page gets 904 bytes of data + 3192 bytes zeroed) before promoting to Ready. Non-negotiable security contract — prevents stale frame data leaking to userspace.

### Three-Layer FD Model (BSD-style)
- **Layer 1 — FdTable** (per-process): `SpinMutex<Vec<Option<(Arc<FileDescription>, FdFlags)>>>`. Index = fd number. FdFlags (bitflags) stored per-slot, not per-file.
- **Layer 2 — FileDescription** (shared across dup/fork): `Arc<FileDescription>` with `offset: AtomicU64`, `flags: AtomicU32`, `file_object: FileObject`. Shared between parent/child after fork, between original/dup'd fds.
- **Layer 3 — FileObject** (the backing resource): `enum FileObject { Vnode(Arc<Vnode>), Pipe(Arc<PipeEnd>), Device(DeviceNode) }`. Devices are peers of Vnode and Pipe, not subtypes.

### Close-on-exec & FD Inheritance
- **FdFlags**: `bitflags! { CLOEXEC = 0o2000000 }` per fd slot in FdTable.
- **dup/dup2**: Clone `Arc<FileDescription>`, clear CLOEXEC on new fd (POSIX requirement).
- **dup3**: Clone Arc, set CLOEXEC if O_CLOEXEC passed.
- **pipe2(O_CLOEXEC)**: Both read and write fds get CLOEXEC set.
- **fork**: Clone all entries — `Arc::clone` + copy FdFlags to child's FdTable.
- **exec**: Past point-of-no-return, iterate entries, `.take()` all slots with CLOEXEC set. Drop triggers resource cleanup.

### execve User Stack ABI (RISC-V Sv39)
- **Byte-level layout** (high address → low address):
  1. String payloads (envp strings, argv strings, null-terminated)
  2. Padding (0–15 bytes to ensure final sp is 16-byte aligned)
  3. Auxiliary vectors (`{u64 a_type, u64 a_val}` pairs: AT_PAGESZ, AT_RANDOM, terminated by AT_NULL)
  4. envp pointer array (terminated by NULL)
  5. argv pointer array (terminated by NULL)
  6. argc (u64)
- **sp**: Points to argc. Must be 16-byte aligned. Computed by subtracting total payload size from stack top and rounding down to 16.
- **Register contract**: `a0 = 0` (no dynamic linker cleanup function). All startup data delivered via sp-pointed memory.
- **Alignment failure = instant death**: musl `_start` asm triggers alignment fault or segfault if sp is not 16-byte aligned.

### Syscall Surface
- **mmap/munmap/mprotect**: As described above. MAP_ANONYMOUS, MAP_PRIVATE, MAP_FIXED.
- **brk**: In-place VmArea expand/shrink.
- **dup/dup2/dup3**: With CLOEXEC semantics as described.
- **lseek**: Adjust FileDescription offset. SEEK_SET, SEEK_CUR, SEEK_END.
- **fstat**: Return stat struct from vnode metadata.
- **clock_gettime**: CLOCK_MONOTONIC and CLOCK_REALTIME via rdtime CSR.
- **nanosleep**: Async sleep via executor timer wheel. Interruptible by signals (returns EINTR with remaining time in `rem`).

### Devfs & Device Nodes
- **Static device table**: No full devfs filesystem. Static table of DeviceNode entries with name + enum dispatch. VFS lookup for `/dev/*` paths hits this table. Devices: null, zero, console.
- **FileObject::Device variant**: Devices are peers of Vnode and Pipe in the FileObject enum, not subtypes of Vnode. Matches FreeBSD's DTYPE_PIPE/DTYPE_VNODE separation.
- **/dev/null**: Read returns 0 (EOF). Write discards (`uio_resid = 0`).
- **/dev/zero**: Read fills buffer with zeros via uiomove from pre-zeroed region. Write discards.
- **/dev/console**: SBI ecall backend.
  - **Write**: Synchronous SBI putchar in a loop (pseudo-non-blocking, fast on QEMU).
  - **Read**: `ConsoleReadFuture` — try `sbi_console_getchar()`, if no data → register 10ms timer wheel wakeup → return `Poll::Pending`. 100Hz async polling. **Critical**: SBI has no IRQ for input — naive park = permanent deadlock (waker starvation). Timer wheel is the pseudo-IRQ source. Marked as tech debt — VirtIO console driver replaces this later.
- **ioctl surface**: Stub termios + TIOCGWINSZ + FIONBIO.
  - **TCGETS**: copyout stored `SpinMutex<Termios>` (initialized with ICANON|ECHO, B38400). Makes `isatty()` return 1 for glibc/FreeBSD libc (which use `tcgetattr`).
  - **TCSETS/TCSETSW/TCSETSF**: copyin user Termios, store internally. No actual hardware effect — "data echo" stub.
  - **TIOCGWINSZ**: copyout static 80x24 `winsize`. Makes `isatty()` return 1 for musl (which uses TIOCGWINSZ, NOT TCGETS — verified against Linux kernel changelog).
  - **FIONBIO**: Accept silently.

### Claude's Discretion
- Exact ring buffer implementation details (head/tail pointers vs index arithmetic)
- Signal pending set data structure (bitmap vs bitflags)
- Futex hash table sizing
- Auxiliary vector entries beyond AT_PAGESZ/AT_RANDOM in exec
- Exact timer wheel tick period (10ms suggested, tunable)
- pgrp index structure (linear scan vs HashMap<pgid, Vec<TaskRef>>)

</decisions>

<specifics>
## Specific Ideas

- FreeBSD's `sendsig()` verified as reference for signal frame construction (`sys/riscv/riscv/exec_machdep.c:337`)
- FreeBSD's `pipe_write()` verified for PIPE_BUF atomicity (`sys/kern/sys_pipe.c:1261-1263`)
- FreeBSD's `struct file` with `f_type`/`f_ops`/`f_data` verified as reference for three-layer fd model (`sys/sys/file.h`)
- FreeBSD's `/dev/null` implementation verified (`sys/dev/null/null.c`) — `null_write` sets `uio_resid = 0`, `zero_read` uses `uiomove` from `zero_region`
- FreeBSD's `vm_mmap_object()` → `vm_map_fixed()` → `vm_map_delete()` verified for MAP_FIXED overwrite behavior
- FreeBSD address space layout: stack at top, mmap top-down, brk up from .bss — verified as reference for VA zone separation
- musl `isatty()` uses `ioctl(TIOCGWINSZ)`, NOT `tcgetattr`/`TCGETS` — verified via Linux kernel changelog. Both TIOCGWINSZ and TCGETS needed for musl + busybox compatibility.
- Console read future must use timer wheel as pseudo-IRQ to avoid waker starvation in async executor
- RISC-V ecall is fixed 4 bytes — restart via `sepc -= 4` is the hardware-level PC rewind, not a kernel-side retry loop
- nanosleep EINTR must write remaining time to user `rem` pointer — POSIX strict requirement, never auto-restart

</specifics>

<deferred>
## Deferred Ideas

- **M:N pipe waker topology**: `Option<Waker>` sufficient for 1:1 shell pipes. Multi-reader/writer pipes need dedup wait queue — future phase when web server / worker pool patterns are needed.
- **VirtIO console driver**: Replace SBI polling with interrupt-driven I/O for proper async console.
- **One-shot timer precision**: Upgrade timer wheel to support hardware timer reprogramming for sub-tick nanosleep accuracy.
- **Full termios state machine**: Real echo, line buffering, signal character processing (Ctrl+C → SIGINT). Current stub just stores/returns the struct.
- **MAP_SHARED**: Requires writeback state machine and page cache dirty tracking. Deferred to avoid complexity.
- **Full job control**: Sessions (`setsid`), controlling terminal (`tcsetpgrp`), SIGTTIN/SIGTTOU for background process TTY access. Requires TTY line discipline rewrite.

</deferred>

---

*Phase: 04-pipes-signals-full-syscalls*
*Context gathered: 2026-02-25*
