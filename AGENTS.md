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
    proc/        — Process management (fork, exec, wait, signals)
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
| `fork-hardcopy` | Use deep-copy fork instead of CoW |

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
