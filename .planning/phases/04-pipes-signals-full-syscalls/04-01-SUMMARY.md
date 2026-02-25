---
phase: 04-pipes-signals-full-syscalls
plan: 01
subsystem: fs, proc
tags: [fd-table, pipe, devfs, dup, ioctl, cloexec]

requires:
  - phase: 03-exception-fixup-vfs-disk-process
    provides: FdTable with OpenFile, Vnode trait, syscall dispatch, fork/exec
provides:
  - Three-layer FD model (FdTable -> FileDescription -> FileObject)
  - Pipe with 4KB ring buffer and async waker integration
  - /dev/null, /dev/zero, /dev/console device nodes
  - dup/dup2/dup3 with CLOEXEC semantics
  - pipe2 syscall
  - ioctl stubs for isatty() (TCGETS, TIOCGWINSZ)
  - CLOEXEC stripping on exec
  - Stdio pre-population (fd 0/1/2)
affects: [04-02-signals, 04-03-remaining-syscalls]

tech-stack:
  added: []
  patterns: [three-layer-fd-model, async-pipe-futures]

key-files:
  created:
    - kernel/src/fs/pipe.rs
    - kernel/src/fs/devfs.rs
  modified:
    - kernel/src/fs/fd_table.rs
    - kernel/src/fs/mod.rs
    - kernel/src/fs/syscalls.rs
    - kernel/src/proc/task.rs
    - kernel/src/proc/fork.rs
    - kernel/src/proc/exec.rs
    - kernel/src/executor/user_task.rs
    - hal-common/src/errno.rs

key-decisions:
  - "All three tasks (FD model, pipes, devices) implemented as single atomic commit — tightly coupled"
  - "PipeReadFuture/PipeWriteFuture as explicit Future impls for async pipe I/O"
  - "Console read returns 0 (EOF) for now — no input buffer until signal infrastructure"
  - "ioctl writes directly to user memory via ptr::copy_nonoverlapping (console fds only)"

patterns-established:
  - "Three-layer FD: FdTable(per-process) -> FileDescription(Arc-shared) -> FileObject(enum)"
  - "Async pipe: register waker WHILE holding lock, wake AFTER releasing lock"
  - "Device dispatch: match on DeviceKind in read/write paths"

requirements-completed: [IPC-01, SYS-01, SYS-06]

duration: 8min
completed: 2026-02-25
---

# Plan 04-01: Three-Layer FD Model + Pipes + Device Nodes + dup

**BSD three-layer FD model replaces OpenFile, with pipes, device nodes, dup/dup2/dup3, pipe2, and ioctl stubs for isatty()**

## Performance

- **Duration:** 8 min
- **Started:** 2026-02-25T15:08:54Z
- **Completed:** 2026-02-25T15:16:26Z
- **Tasks:** 3 completed
- **Files modified:** 10

## Accomplishments
- Replaced flat OpenFile-based fd table with three-layer BSD model (FdTable -> FileDescription -> FileObject)
- Pipe with 4KB ring buffer, async waker-based read/write futures, EPIPE on dead reader, EOF on dead writer
- /dev/null, /dev/zero, /dev/console as static device nodes with ioctl support (TCGETS, TIOCGWINSZ)
- dup/dup2/dup3 with CLOEXEC, pipe2 with O_CLOEXEC, exec strips CLOEXEC fds
- All WRITE/READ syscalls now dispatch through FileObject (Console, Pipe, Vnode, Device)

## Task Commits

All tasks committed atomically (tightly coupled):

1. **T1+T2+T3: Three-layer FD model + pipes + devices** - `35a467c` (feat)

## Files Created/Modified
- `kernel/src/fs/fd_table.rs` - Rewritten: FdTable/FileDescription/FileObject/FdFlags/DeviceKind
- `kernel/src/fs/pipe.rs` - New: 4KB ring buffer pipe with async waker integration
- `kernel/src/fs/devfs.rs` - New: static device node lookup (/dev/null, /dev/zero, /dev/console)
- `kernel/src/fs/mod.rs` - Added pipe, devfs modules
- `kernel/src/fs/syscalls.rs` - Updated for three-layer model, /dev/ prefix handling
- `kernel/src/executor/user_task.rs` - Rewrote WRITE/READ through FD model, added DUP/DUP3/PIPE2/IOCTL
- `kernel/src/proc/task.rs` - Init uses new_with_stdio()
- `kernel/src/proc/fork.rs` - Fork works with new FdTable (no changes needed)
- `kernel/src/proc/exec.rs` - Added strip_cloexec after point-of-no-return
- `hal-common/src/errno.rs` - Added EPIPE, ENOTTY, EMFILE, EEXIST

## Decisions
- All three plan tasks implemented as single commit — the FD model, pipes, and devices are interdependent
- PipeReadFuture/PipeWriteFuture as explicit Future impls rather than async fn (needed for waker registration)
- Console read returns EOF for now (no input buffer until signal infrastructure exists)

## Verification
- `make kernel-rv64` compiles clean
- `make test` passes 96/96 host unit tests

---
*Plan: 04-01 | Phase: 04-pipes-signals-full-syscalls*
*Completed: 2026-02-25*
