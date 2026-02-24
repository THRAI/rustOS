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

### Pipe & Futex Design
- **Fixed 4KB ring buffer**: No dynamic resizing. Writer returns `Poll::Pending` when full, reader returns `Poll::Pending` when empty. `IrqSafeSpinLock` protects `PipeInner`.
- **Waker storage**: `Option<Waker>` per end (not `Vec<Waker>`). Unconditional overwrite on each poll — zero leak, zero `will_wake` scan. Sufficient for 1:1 shell pipe topology. **Tech debt**: M:N multi-reader pipes need upgrade to dedup wait queue in future phase.
- **Wake ordering**: Register waker WHILE holding lock (atomicity with emptiness/fullness check). Wake AFTER releasing lock (avoid thundering herd under lock). Prevents lost wakeups.
- **PIPE_BUF atomic write**: Writes ≤ PIPE_BUF (4096): if `free < data.len()`, return `Poll::Pending` without writing anything — all-or-nothing. Writes > PIPE_BUF: partial writes allowed. Verified against FreeBSD `sys/kern/sys_pipe.c:1261-1263`.
- **SIGPIPE death detection**: Reader Drop sets `reader_closed = true` + wakes all sleeping writers. Writer poll sees `reader_closed` → returns `EPIPE` → syscall layer posts SIGPIPE to calling task → delivered at userret.
- **Futex**: Process-private only. Keyed by **physical address** (walk page table on trap entry to get PA). Global `HashMap<PhysAddr, Vec<Waker>>`. FUTEX_WAIT: atomic compare + park. FUTEX_WAKE: wake N waiters.

### Three-Layer FD Model (BSD-style)
- **Layer 1 — FdTable** (per-process): `SpinMutex<Vec<Option<(Arc<FileDescription>, FdFlags)>>>`. Index = fd number. `FdFlags` is per-slot (contains FD_CLOEXEC). Never shared across fork — each process gets its own FdFlags copy.
- **Layer 2 — FileDescription** (shared via Arc on dup/fork): Contains `FileObject` + `SpinMutex<u32>` (file status flags: O_NONBLOCK, O_APPEND) + `SpinMutex<u64>` (offset, only meaningful for vnodes).
- **Layer 3 — FileObject** (enum): `Vnode(Arc<dyn Vnode>)`, `Pipe(Arc<PipeEnd>)`, `Device(DeviceType)`. `match desc.object` routes read/write/ioctl.
- **POSIX flag separation**: File Status Flags (O_NONBLOCK, O_APPEND) → `FileDescription::flags` (shared). File Descriptor Flags (FD_CLOEXEC) → `FdTable` slot-local (per-process, never shared). Critical for fork safety.
- **dup/dup2/dup3**: `Arc::clone(entries[old])` into new slot. dup2 atomically replaces target slot under single SpinMutex. Old FileDescription dropped outside lock — actual close runs when last Arc ref drops.
- **O_CLOEXEC sweep in exec**: After point-of-no-return (old VmMap destroyed), iterate FdTable entries, `.take()` any slot with `FdFlags::CLOEXEC` set.

### Syscall Surface & mmap
- **Full mmap** (anonymous + file-backed):
  - `sys_mmap` is pure metadata — parse FD, validate alignment, insert VMA into VmMap, return VA. Zero I/O, zero physical allocation. Demand paging handles the rest.
  - **MAP_SHARED**: pmap_enter directly maps Page Cache physical frame to user PTE with R|W. Dirty bit tracked by hardware. Writeback via lwext4 on msync/munmap. Cache coherence is free — sys_read's uiomove reads from the same Page Cache frame.
  - **MAP_PRIVATE**: Read fault maps Page Cache frame as read-only. Write fault triggers COW — allocate anonymous page, copy data, map R|W, attach to shadow object. Write never modifies underlying file.
- **munmap/mprotect/brk**: Complete implementation for malloc-heavy workloads.
- **clock_gettime**: `rdtime` CSR direct (RISC-V cycle counter, S-mode accessible). Convert to nanoseconds via `timebase-frequency` from FDT. No SBI ecall overhead.
- **nanosleep**: Async sleep via executor timer wheel. Tick-granularity resolution (bounded by timer IRQ period). Interruptible by signals (returns EINTR with remaining time). Tech debt: upgrade to one-shot timer for sub-tick precision if needed later.

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
- Auxiliary vector entries beyond AT_PAGESZ in exec
- Exact timer wheel tick period (10ms suggested, tunable)

</decisions>

<specifics>
## Specific Ideas

- FreeBSD's `sendsig()` verified as reference for signal frame construction (`sys/riscv/riscv/exec_machdep.c:337`)
- FreeBSD's `pipe_write()` verified for PIPE_BUF atomicity (`sys/kern/sys_pipe.c:1261-1263`)
- FreeBSD's `struct file` with `f_type`/`f_ops`/`f_data` verified as reference for three-layer fd model (`sys/sys/file.h`)
- FreeBSD's `/dev/null` implementation verified (`sys/dev/null/null.c`) — `null_write` sets `uio_resid = 0`, `zero_read` uses `uiomove` from `zero_region`
- musl `isatty()` uses `ioctl(TIOCGWINSZ)`, NOT `tcgetattr`/`TCGETS` — verified via Linux kernel changelog. Both TIOCGWINSZ and TCGETS needed for musl + busybox compatibility.
- Console read future must use timer wheel as pseudo-IRQ to avoid waker starvation in async executor

</specifics>

<deferred>
## Deferred Ideas

- **M:N pipe waker topology**: `Option<Waker>` sufficient for 1:1 shell pipes. Multi-reader/writer pipes need dedup wait queue — future phase when web server / worker pool patterns are needed.
- **VirtIO console driver**: Replace SBI polling with interrupt-driven I/O for proper async console.
- **One-shot timer precision**: Upgrade timer wheel to support hardware timer reprogramming for sub-tick nanosleep accuracy.
- **Full termios state machine**: Real echo, line buffering, signal character processing (Ctrl+C → SIGINT). Current stub just stores/returns the struct.

</deferred>

---

*Phase: 04-pipes-signals-full-syscalls*
*Context gathered: 2026-02-25*
