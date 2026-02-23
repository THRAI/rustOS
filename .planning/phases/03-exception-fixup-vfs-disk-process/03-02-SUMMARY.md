---
phase: 03-exception-fixup-vfs-disk-process
plan: 02
subsystem: drivers, fs
tags: [virtio-blk, ext4, delegate-thread, block-io, mmio, bounded-channel]

requires:
  - phase: 03-01
    provides: "Exception fixup, frame allocator, executor, trap infrastructure"
provides:
  - "VirtIO-blk MMIO driver with sector read/write and adaptive polling"
  - "Pure-Rust read-only ext4 parser (superblock, group descriptors, inodes, directories)"
  - "Delegate task serializing ext4 operations off the async executor via bounded channel"
  - "ReplySlot oneshot mechanism for async request/response"
  - "ext4 test disk image creation scripts"
affects: [03-04, 03-05]

tech-stack:
  added: []
  patterns: ["VirtIO MMIO transport with descriptor ring", "delegate task with bounded channel for sync library serialization", "ReplySlot oneshot for async/sync bridge"]

key-files:
  created:
    - kernel/src/drivers/virtio_blk.rs
    - kernel/src/drivers/virtio_mmio.rs
    - kernel/src/fs/mod.rs
    - kernel/src/fs/ext4.rs
    - kernel/src/fs/delegate.rs
    - scripts/make_test_img.sh
    - scripts/make_ext4_img.py
  modified:
    - kernel/src/drivers/mod.rs
    - kernel/src/main.rs
    - Makefile

key-decisions:
  - "Pure-Rust ext4 parser instead of lwext4 FFI — avoids C dependency, sufficient for read-only boot path"
  - "Delegate as async task (not OS thread) — reuses executor infrastructure, yields cooperatively"
  - "ReplySlot with AtomicBool + IrqSafeSpinLock for oneshot wakeup — no alloc per request"
  - "Adaptive polling in VirtIO-blk: spin loop then WFI for completion"
  - "Python script for ext4 image creation — works on macOS without mkfs.ext4"

patterns-established:
  - "VirtIO MMIO probe: scan known base addresses, check magic/version/device_id"
  - "Delegate pattern: single async task owns resource, processes requests from bounded channel"
  - "Block read callback: ext4 parser calls closure that routes to VirtIO-blk read_sector"

requirements-completed: [VFS-06, VFS-07, VFS-08]

duration: 8min
completed: 2026-02-23
---

# Plan 03-02: VirtIO-blk + ext4 + Delegate Thread Summary

**VirtIO-blk MMIO driver reads sectors from QEMU virt machine, pure-Rust ext4 parser mounts filesystem, delegate task serializes all FS operations off the executor — verified by QEMU test reading /hello.txt.**

## Performance

- **Duration:** 8 min
- **Tasks:** 2
- **Files modified:** 10

## Accomplishments
- VirtIO-blk driver discovers block device by probing MMIO addresses, negotiates features, and performs sector I/O via descriptor ring with adaptive polling
- Pure-Rust ext4 parser reads superblock, group descriptors, inodes, and directory entries — no C FFI needed
- Delegate task with bounded channel (capacity 256) serializes ext4 operations without blocking the executor
- QEMU integration test reads "hello from ext4" from /hello.txt through the full stack (16/16 tests pass)

## Task Commits

1. **Task 1: VirtIO-blk MMIO driver** - `a293122` (feat)
2. **Task 2: ext4 reader + delegate thread + integration test** - `2c6a12c` (feat)

## Files Created/Modified
- `kernel/src/drivers/virtio_blk.rs` - VirtIO-blk MMIO driver with read_sector/write_sector and adaptive polling
- `kernel/src/drivers/virtio_mmio.rs` - VirtIO MMIO transport register interface
- `kernel/src/drivers/mod.rs` - Driver module declarations
- `kernel/src/fs/ext4.rs` - Pure-Rust read-only ext4 parser (superblock, inodes, directories, file read)
- `kernel/src/fs/delegate.rs` - Delegate task with bounded channel, ReplySlot oneshot, fs_open/fs_read/fs_close API
- `kernel/src/fs/mod.rs` - Filesystem module declarations
- `kernel/src/main.rs` - VirtIO-blk init, delegate init, test_delegate_read integration test
- `Makefile` - QEMU disk drive flags, "delegate read PASS" test pattern
- `scripts/make_test_img.sh` - Shell script to create 32MB ext4 test image
- `scripts/make_ext4_img.py` - Python ext4 image builder (macOS compatible)

## Decisions
- Chose pure-Rust ext4 parser over lwext4 FFI bindings — eliminates C toolchain dependency, sufficient for read-only boot path, and simpler to maintain
- Delegate runs as an async task (not a dedicated OS thread) — reuses the existing executor, yields cooperatively between operations
- ReplySlot uses AtomicBool + IrqSafeSpinLock instead of heap-allocated oneshot — zero allocation per request
- Python-based ext4 image creation works on macOS without requiring mkfs.ext4 or Docker

## Deviations
- **lwext4 FFI replaced with pure-Rust ext4 parser** — plan specified lwext4 FFI bindings, but a pure-Rust implementation avoids C dependency complexity while meeting all read-only requirements. The delegate pattern and async API remain identical to the plan.

## Next Phase Readiness
- Block I/O and filesystem reading are operational — VFS layer (plan 03-04) can build on the delegate API
- fs_open/fs_read/fs_close async API is ready for syscall wiring in plan 03-05

---
*Phase: 03-exception-fixup-vfs-disk-process*
*Completed: 2026-02-23*
