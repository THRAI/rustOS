# AGENTS.md — BSD-Async Rust OS Kernel

> Instructions for autonomous AI agents working on this codebase.

## Project Overview

A RISC-V 64-bit operating system kernel written in Rust (`no_std`), inspired by FreeBSD's
architecture. Targets `riscv64gc-unknown-none-elf`. Runs on QEMU `virt` machine.

## Repository Layout

```
kernel/          — Main kernel crate (the only workspace member)
  src/
    main.rs      — Entry point, boot, kernel_main, qemu-test harness
    hal/         — Hardware abstraction layer (RISC-V specific)
    hal_common/  — Platform-agnostic HAL traits and types
    mm/          — Memory management (pmap, vm, buddy allocator)
      pmap/      — Hardware page table (Sv39) as a Rust container
        mod.rs       — Pmap struct, get/entry/entry_or_insert/range/for_each_in_range_mut
        container.rs — PmapRef, PmapEntry, OccupiedEntry, VacantEntry, OccupiedEntryMut, PmapRange
        shootdown.rs — TLB shootdown (ShootdownBatch, QuiesceGuard, per-CPU IPI)
        walk.rs      — Generic page table walker
        pte.rs       — PTE encoding/decoding, PteFlags
        asid.rs      — ASID allocator
      vm/        — Virtual memory subsystem
        page/        — Physical page lifecycle
          vm_page.rs     — VmPage metadata, busy-lock state machine
          page_ref.rs    — PageRef RAII frame handle
          wait_queue.rs  — Hashed waker queue for VmPage busy-state
        object/      — VmObject + backing store
          vm_object.rs   — VmObject, VObjIndex, shadow chains, page cache
          pager.rs       — Pager trait, AnonPager, VnodePager
          page_daemon.rs — Dirty-page writeback daemon
        map/         — Address space map (VmMap)
          mod.rs         — VmMap: insert, remove, lookup, protect, fork, mmap
          entry.rs       — VmMapEntry, VmMapping enum, MapPerm, CowState
          splay.rs       — Intrusive splay tree
        fault/       — Page fault handling
          sync.rs        — Synchronous fault handler (trap-stack safe)
          async_resolve.rs — Async fault handler (yields for I/O)
    proc/        — Process management (clone, exec, wait, signals)
      clone.rs   — do_clone(), CloneFlags, VforkDone (replaces fork.rs)
      exec.rs    — do_execve (ELF loading)
      signal.rs  — POSIX signals, sigcode VMA (SIGCODE_OBJ)
      task.rs    — Task struct (Arc-wrapped vm_map, fd_table)
    syscall/     — Syscall dispatch and handlers
    fs/          — VFS layer, ext4 via lwext4_rust
    ipc/         — IPC (futex)
    drivers/     — Block device drivers (virtio)
    executor/    — Async task executor
    console.rs   — UART / SBI console
    trap.rs      — Trap handler entry
    feature_flags.rs — Runtime feature flag registry
    alloc_early.rs   — Early-boot allocator (before buddy init)
    libc_stubs.rs    — C library stubs for lwext4_rust linkage
user/            — Userspace init process (initproc)
scripts/         — Build helpers, QEMU runner, test images
judge/           — Automated scoring for OS competition tests
testcase/        — Test program binaries
ai/              — AI-generated architecture analysis and planning docs
concept_docs/    — High-level project concept documents
freebsd-src/     — Partial FreeBSD source tree (C reference)
                   See freebsd-src/AGENTS.md for BSD architecture reference
                   covering VM, threading, and kqueue/knote subsystems.
                   Consult it when working on mm/, proc/, ipc/, or
                   implementing event-driven / async mechanisms.
chronix/         — Reference OS kernel project (competition entry)
delonix/         — Reference OS kernel project (competition entry)
firmware/        — (reserved, currently empty)
.planning/       — Project planning, roadmap, and milestone tracking
.concept-dev/    — Concept development state and assumptions
clippy.toml      — Clippy thresholds (complexity, args, etc.)
rustfmt.toml     — Code formatting configuration
.githooks/       — Git hooks (pre-commit: fmt, large files, secrets)
.github/         — CI workflows, Dependabot
```

## Toolchain & Prerequisites

```bash
# Install the exact nightly toolchain (defined in rust-toolchain.toml)
rustup show                     # auto-installs from rust-toolchain.toml
# Toolchain: nightly-2025-06-01
# Components: rust-src, llvm-tools, rustfmt, clippy
# Targets: riscv64gc-unknown-none-elf, loongarch64-unknown-none

# Additional host tools
cargo install cargo-binutils
# QEMU for running the kernel
# macOS: brew install qemu
# Linux: sudo apt install qemu-system-riscv64 e2fsprogs
```

### Cargo Config (`.cargo/config.toml`)

- Default build target: `riscv64gc-unknown-none-elf`
- Linker script: `kernel/linker/rv64-qemu.ld`
- Rustflags: frame pointers forced on, RISC-V atomics extensions (`zaamo`, `zalrsc`)

## Build Commands

```bash
# Build kernel ELF (autotest mode — runs test scripts then shuts down)
make all                        # → produces ./kernel-rv

# Build kernel binary image for interactive QEMU
make kernel-rv64                # → produces kernel-rv64.bin

# Build with specific log modules (comma-separated)
make kernel-rv64 LOG=fs,vm LEVEL=debug

# Build kernel with qemu-test feature (in-kernel test harness)
make kernel-rv64-test

# Build kernel with autotest feature (auto-run tests then shutdown)
make kernel-rv64-autotest

# Build user-space initproc
make user-rv64

# Build user-space initproc with autotest feature
make user-rv64-autotest

# Build test disk image (scripts/test.img)
make disk-img
```

## Running

```bash
# Interactive QEMU session (uses pexpect-based test runner)
make run-rv64

# OS competition standard test
make run-oscomp

# OS competition convenience targets (build image + run)
make oscomp                     # full test suite
make oscomp-basic               # basic tests only
make oscomp-basic-all           # basic-musl + basic-glibc

# QEMU integration tests (pattern-matching on output)
make qemu-test-rv64

# Quick agent smoke test (15s timeout)
make agent-test

# Python-based interactive QEMU test
make python-test-rv64

# GDB debug: halt on start, GDB server on port 1234
make debug-rv64

# GDB server without halt (attach to running kernel)
make gdbserver-rv64
```

## Testing

```bash
# QEMU integration tests (boot kernel, match expected output patterns)
make qemu-test-rv64

# Agent smoke test (faster, fewer patterns)
make agent-test

# Python-based interactive QEMU integration test (recommended for validating
# end-to-end boot + userspace functionality after kernel changes)
make python-test-rv64

# Run all tests
make test-all

# OS competition scoring
./judge/batch_judge.sh
./judge/batch_judge.sh --target oscomp-basic-all

# Build OS competition disk image (requires Linux for ext4 mount)
make sdcard-rv
```

### `make python-test-rv64` — Interactive Integration Test

The most thorough integration test available. It boots the kernel in QEMU,
waits for the shell prompt, then sends commands from
`scripts/intergration_command` (one per line) via a pexpect-based test
runner (`scripts/test_runner.py`).

**What it validates:**
- Full boot sequence (FDT parse → frame allocator → VirtIO → ext4 mount → init)
- Shell prompt appears (initproc / busybox started successfully)
- Each command executes without crashing (prompt returns after each)
- Pipes and process lifecycle work (`echo | cat | wc` pipeline)

**How it works:**
1. Builds `kernel-rv64` and the disk image (`scripts/test.img`)
2. Spawns QEMU with the kernel and disk attached
3. Waits for `# ` shell prompt (30s timeout)
4. For each line in `scripts/intergration_command`:
   - Sends the command character-by-character (3ms delay to avoid UART FIFO overrun)
   - Waits for the next `# ` prompt (30s timeout)
5. Exits 0 on success, 1 on timeout or unexpected QEMU exit

**Adding test commands:** Append lines to `scripts/intergration_command`.
Lines starting with `#` are ignored. Each line is a shell command that
must complete and return to the prompt within the timeout.

**Options** (pass via environment or edit `scripts/test_runner.py`):
- `--timeout N` — seconds to wait for each prompt (default: 30)
- `--interactive` — drop to a live QEMU shell instead of running commands
- `--char-delay F` — seconds between each character sent (default: 0.003)

There are no host-side unit tests for the kernel crate (it is `no_std`, binary-only).
Some modules in `hal_common` have `#[cfg(test)]` tests that can run with:
```bash
cargo test -p kernel --target $(rustc -vV | grep host | cut -d' ' -f2) 2>/dev/null || true
```

Concurrency tests using `loom` are available as dev-dependencies for host-target builds.

## Code Quality

```bash
# Format check
cargo fmt --check

# Lint (workspace clippy rules defined in Cargo.toml [workspace.lints.clippy])
cargo clippy --target riscv64gc-unknown-none-elf -p kernel -- -D warnings

# Pre-commit hooks (install once)
git config core.hooksPath .githooks
```

### Naming Conventions

- **Modules & files**: `snake_case` (e.g., `hal_common`, `feature_flags.rs`)
- **Types & traits**: `PascalCase` (e.g., `ProcState`, `VmObject`)
- **Functions & methods**: `snake_case` (e.g., `do_fork`, `sys_write`)
- **Syscall handlers**: `sys_<name>` (e.g., `sys_openat`, `sys_mmap`)
- **Constants**: `SCREAMING_SNAKE_CASE`
- **Feature flags** (compile-time): kebab-like in Cargo.toml (`log-boot`, `qemu-test`)
- **Unsafe blocks**: must have a `// SAFETY:` comment explaining invariants

## Cargo Features (compile-time)

| Feature | Purpose |
|---------|---------|
| `qemu-test` | Enable in-kernel test harness functions |
| `autotest` | Boot -> run test script -> shutdown |
| `log-{module}` | Selective logging: `boot`, `syscall`, `trap`, `vm`, `sched`, `fs`, `driver`, `smp`, `signal`, `exec`, `pipe`, `proc` |
| `log-all` | Enable all log modules |
| `log-level-{level}` | Hierarchical: `error`, `warn` (implies error), `info` (implies warn), `debug` (implies info), `trace` (implies debug) |
| `log-level-all` | Enable all log levels (implies trace) |
| `exec-legacy` | Use legacy exec path |

## Runtime Feature Flags

The kernel supports runtime feature flags via `kernel/src/feature_flags.rs`.
Flags are stored as bits in an `AtomicU32`. See the `Flag` enum:

| Flag | Default | Description |
|------|---------|-------------|
| `CowFork` | Enabled | Use CoW-optimised fork path instead of deep-copy fallback |
| `AsyncVfsIo` | Disabled | Enable async I/O path for VFS read/write |
| `SyscallTrace` | Disabled | Verbose syscall tracing at runtime (independent of log features) |
| `NewSignalDelivery` | Disabled | Use the new signal delivery mechanism |
| `MmapOptimised` | Disabled | Enable experimental mmap optimisations |

## Architecture Notes

- **SMP**: Up to 4 harts (configurable via `SMP=N`)
- **Memory**: 128MB RAM, buddy allocator + per-process page tables
- **Filesystem**: ext4 via `lwext4_rust` (mounted at `/`)
- **Async**: Custom executor for kernel async tasks
- **Signals**: POSIX-like signal model
- **No `std`**: Everything is `#![no_std]`, no heap until buddy allocator is initialized

### VM / Pmap Architecture

The virtual memory subsystem has four layers:

```
┌──────────────────────────────────────────────────────────────┐
│  Callers (sys_mmap, do_execve, map_sigcode)                  │
│    Build VmObject, choose VmMapping variant                  │
│    Call: VmMap.map(start, end, perm, VmMapping::*)            │
├──────────────────────────────────────────────────────────────┤
│  VmMap Builder + Business Methods                            │
│    map()           — construct entry from VmMapping enum      │
│    cow_fork_into() — COW fork (ForkDescriptor pipeline)       │
│    share_into()    — shared address space (CLONE_VM)          │
│    map_sigcode()   — sigcode VMA via VmMapping::Shared        │
├──────────────────────────────────────────────────────────────┤
│  VmMap Accountant (CRUD + pmap bind + invariant enforcement) │
│    insert_entry, remove_range, protect_range                 │
│    grow_heap, shrink_heap, clear                             │
│    rebind_store, set_cow_state                               │
│    split_entry_at (shared geometry for range ops)             │
├──────────────────────────────────────────────────────────────┤
│  Pmap Container (hardware page table cache)                  │
│    get(va) → PmapRef          (read-only lookup)             │
│    entry(va) → Occupied|Vacant (mutable entry API)           │
│    range(start, end) → Iterator (subtree-skipping)           │
│    for_each_in_range_mut()    (mutable range visitor)         │
│    ShootdownBatch             (quiesce-on-create, flush-drop) │
└──────────────────────────────────────────────────────────────┘
```

**Key types:**

- **`VmMapping`** enum (6 variants: `AnonPrivate`, `FilePrivate`, `FileShared`,
  `Heap`, `Shared`, `Guard`) — stored directly in `VmMapEntry`. Each variant
  carries its `VmObject`. Inheritance is derived from the variant discriminant
  via `mapping.inheritance()`. Replaces the old `BackingStore` + `EntryFlags` +
  `VmInherit` triple.

- **`CowState`** (`None` | `CopyOnWrite`) — the only independent mutable axis
  on `VmMapEntry`, set by `cow_fork_into` during fork.

- **`ForkDescriptor`** (`Share` | `Cow`) — pure classification of per-VMA fork
  action. `cow_fork_into` is a pipeline: `iter → filter_map(classify) → collect
  → try_for_each(apply)` under one `ShootdownBatch`.

- **`PmapEntry`** (`Occupied` | `Vacant`) — Rust Entry API for pmap. Point
  mutations (insert, promote, remove) on single PTEs. `OccupiedEntry` borrows
  the PTE slot, not `&mut Pmap` — no extra locking needed beyond `SpinMutex<Pmap>`.

- **`ShootdownBatch`** — wraps `QuiesceGuard` (parks remote harts on create),
  collects `ShootdownToken`s from PTE mutations, adaptive local flush on drop.

**Invariant:** Every user-space pmap mapping is authorized by a `VmMapEntry`.
No pmap-only mappings exist (sigcode is a proper `VmMapping::Shared` VMA).

### Process Creation (clone)

`do_clone(flags)` in `proc/clone.rs` is the universal process/thread creation
primitive. `fork()` no longer exists. Flag-driven dispatch:

| Flag | Behavior |
|------|----------|
| (none) | Traditional COW fork via `VmMap::cow_fork_into()` |
| `CLONE_VM` | Share address space (`Arc::clone` of `vm_map`) |
| `CLONE_FILES` | Share fd table (`Arc::clone` of `fd_table`) |
| `CLONE_VFORK` | Parent blocks via `VforkDone` until child exits/execs |
| `CLONE_SETTLS` | Set child TLS register |
| `CLONE_THREAD` | Rejected (ENOSYS — future Phase D) |

`Task` struct Arc-wraps `vm_map` and `fd_table` to support sharing.

### Error Encoding Policy

The kernel maintains a single Rust-level error type (`Errno`) and converts
to/from raw integers **only** at three well-defined boundaries.  The full
policy is documented in `kernel/src/hal_common/errno.rs`.

**Rules:**

1. Inside the Rust kernel, all fallible functions return `KernelResult<T>`
   (alias for `Result<T, Errno>`).  Never return a raw `usize` encoding
   an error value.
2. `Errno::as_i32()` returns **positive** POSIX values (e.g. `EINVAL = 22`).
3. `Errno::as_linux_ret()` returns **negative** Linux-convention values
   (e.g. `-22`).  Only the syscall dispatcher should call this.
4. Domain-specific error enums are acceptable when they stay within their
   subsystem and never cross into syscall returns.  Prefer `kerr!` to pair
   every `Errno` with a log at the failure site (see Logging Discipline).

**Boundaries (the only places raw integer errnos appear):**

| Boundary | Direction | Function | Location |
|----------|-----------|----------|----------|
| Syscall return | `Errno` → `a0` register | `syscall_error_return()` | `syscall/mod.rs` |
| lwext4 C FFI | C `int` → `Errno` | `lwext4_err()` | `fs/ext4.rs` |
| copy_user asm | asm `usize` → `Errno` | comparison in `uiomove()` | `mm/uio.rs` |

**Exception:** `sys_brk` returns a plain `usize` (current break address on
failure, never a negative errno) because the Linux brk ABI requires this.

### Logging Discipline

All kernel logging goes through macros defined in `kernel/src/console.rs`.
There are no log sinks, ring buffers, structured logging, or runtime filters
beyond `feature_flags::SyscallTrace`. Output is UART only.

**Macro hierarchy:**

| Macro | Purpose | Gated? |
|-------|---------|--------|
| `kprint!(fmt, args...)` | Raw UART write (no newline) | Never |
| `kprintln!(fmt, args...)` | Raw UART write (with newline) | Never |
| `klog!(mod, level, fmt, args...)` | Structured kernel log | Module + level (compile-time) |
| `kerr!(mod, level, errno, fmt, args...)` | Log + produce `Errno` value | Same as `klog!` |
| `kreturn!(mod, level, errno, fmt, args...)` | Log + `return Err(errno)` | Same as `klog!` (unused, reserved) |

**Two-layer compile-time gating (applied to `klog!`, `kerr!`, `kreturn!`):**

1. **Level gate** — controlled by `log-level-{error,warn,info,debug,trace}` features.
   Levels form an implication chain: `trace` implies `debug` implies `info` implies
   `warn` implies `error`.  If **no** `log-level-*` feature is set, all levels pass
   (default-allow).  Once any level feature is set, only enabled levels pass.
2. **Module gate** — controlled by `log-{module}` features (e.g. `log-vm`, `log-fs`).
   Only messages whose module feature is enabled pass through.

**Special case — `error` level bypasses module gating.** `klog!(vm, error, ...)`
prints even when `log-vm` is disabled.  This is intentional: errors must always
be visible.

**`kerr!` discipline — every `Errno` must be paired with a log:**

When returning an `Errno` from a failure site, use `kerr!` instead of
constructing the errno directly.  This enforces that every error production
is logged at the point where diagnostic context (addresses, pids, etc.) is
available.

```rust
// GOOD — errno and log are inseparable
let frame = alloc_frame().ok_or_else(|| {
    kerr!(vm, error, Errno::Enomem, "OOM: frame alloc pid={} va={:#x}", pid, va)
})?;

// BAD — returns errno with no log, diagnostic info is lost
let frame = alloc_frame().ok_or(Errno::Enomem)?;
```

**Level assignment conventions:**

| Severity | Level | Module-gated? | When to use |
|----------|-------|---------------|-------------|
| Operational failure | `error` | No (always visible) | OOM, I/O failure, invariant violation |
| Suspicious but recoverable | `warn` | Yes | Unexpected state, races, fallback paths |
| Normal flow (verbose) | `debug` | Yes | Permission checks, VMA lookups, expected faults |
| Hot-path tracing | `trace` | Yes | Per-fault flow, per-syscall entry/exit |

**Boot milestone prints use `kprintln!`, not `klog!`:**

Boot milestones that are `agent-test` expectations (e.g. `"delegate running"`,
`"exec OK"`) must use unconditional `kprintln!`.  Using `klog!` would make them
invisible when `LEVEL=error` (the default build), causing test failures.

## CI/CD

- **CI** (`.github/workflows/ci.yml`): `cargo fmt --check`, `clippy`, unused deps (non-blocking via `cargo-udeps`), large file detection (flags `.rs` files over 500 lines), automated PR review (size/quality metrics, unsafe code detection)
- **Release** (`.github/workflows/release.yml`): Tag `v*` -> build kernel ELF + binary -> GitHub Release with auto-generated categorised changelog
- **Dependabot** (`.github/dependabot.yml`): Weekly Cargo dependency updates (Mondays), `lwext4_rust` excluded (pinned to specific branch)

## Common Pitfalls

1. The kernel can only be built with `--target riscv64gc-unknown-none-elf`. Host-target builds will fail (no `std`).
2. `make sdcard-rv` requires Linux (ext4 mount). Use Docker on macOS.
3. Large `unsafe` blocks are common in HAL code — always add `// SAFETY:` comments.
4. `lwext4_rust` is an external C library; do not add Rust-only filesystem abstractions that bypass it.
5. `log` and `lazy_static` are workspace dependencies but may only be used transitively or via `extern crate`.
