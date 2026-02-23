---
phase: 03-exception-fixup-vfs-disk-process
plan: 04
subsystem: fs, mm
tags: [vfs, vnode, page-cache, dentry, fd-table, syscalls, fault-in-page, file-backed]

requires:
  - phase: 03-02
    provides: "VirtIO-blk driver, ext4 parser, delegate task with bounded channel"
  - phase: 03-03
    provides: "Task struct, fork COW, exit/wait4, SyscallResult"
provides:
  - "Vnode trait + Ext4Vnode implementation"
  - "Global dentry cache for path component lookup"
  - "3-state page cache (Absent -> Fetching -> Cached) with CAS transitions"
  - "Per-process FdTable with Arc<OpenFile> entries (shared on fork per POSIX)"
  - "Iterative path walk from root vnode through dentry cache + delegate"
  - "Filesystem syscalls: sys_open, sys_read, sys_write (stub), sys_close, sys_stat"
  - "Async fault_in_page for file-backed pages with boundary anonymization"
  - "VmArea file-backed fields (vnode, file_offset, file_size) for demand-paged ELF loading"
affects: [03-05]

tech-stack:
  added: []
  patterns: ["3-state page cache with CAS transitions", "Vnode trait for filesystem abstraction", "Per-process FdTable with Arc<OpenFile> shared on fork"]

key-files:
  created:
    - kernel/src/fs/vnode.rs
    - kernel/src/fs/dentry.rs
    - kernel/src/fs/page_cache.rs
    - kernel/src/fs/fd_table.rs
    - kernel/src/fs/path.rs
    - kernel/src/fs/syscalls.rs
  modified:
    - kernel/src/fs/mod.rs
    - kernel/src/fs/delegate.rs
    - kernel/src/fs/ext4.rs
    - kernel/src/mm/vm/vm_map.rs
    - kernel/src/mm/vm/fault.rs
    - kernel/src/proc/task.rs
    - kernel/src/proc/fork.rs
    - kernel/src/main.rs
    - Makefile

key-decisions:
  - "Vnode trait with vnode_id/vtype/size — minimal interface, Ext4Vnode sends ops to delegate"
  - "BTreeMap dentry cache (no eviction needed for competition workloads)"
  - "3-state page cache: Absent -> Fetching(WakerList) -> Cached(PhysAddr) with lock-based CAS"
  - "FdTable uses BTreeMap<u32, Arc<OpenFile>> with next_fd starting at 3 (0-2 reserved)"
  - "Boundary page anonymization: fresh frame + memcpy + zero tail, never mutate page cache frames"
  - "Delegate extended with Lookup/Stat/ReadPage request types for VFS path walk and page cache"

patterns-established:
  - "Vnode trait: filesystem-agnostic abstraction for file operations"
  - "Page cache probe/complete: non-blocking probe for hot path, async fetch for cold path"
  - "FdTable fork: Arc-shared OpenFile entries per POSIX semantics"
  - "fault_in_page: async file-backed fault resolution via page cache + delegate"

requirements-completed: [VFS-01, VFS-02, VFS-03, VFS-04, VFS-05, VM-10]

duration: 10min
completed: 2026-02-23
---

# Plan 03-04: VFS Layer Summary

**Vnode trait, 3-state page cache, per-process fd table, filesystem syscalls, and async fault_in_page for file-backed pages — QEMU test reads /hello.txt through VFS with page cache hit on second read (17/17 tests pass).**

## Performance

- **Duration:** 10 min
- **Started:** 2026-02-23T13:05:56Z
- **Completed:** 2026-02-23T13:15:18Z
- **Tasks:** 2
- **Files modified:** 15

## Accomplishments
- Complete VFS layer with Vnode trait, dentry cache, page cache, fd table, path walk, and syscalls
- 3-state page cache correctly transitions Absent -> Fetching -> Cached with waker-based notification
- Async fault_in_page handles file-backed pages with boundary anonymization (partial pages get fresh frame)
- VmArea extended with vnode/file_offset/file_size for demand-paged ELF loading in plan 03-05
- 17/17 QEMU integration tests pass, 86/86 host unit tests pass

## Task Commits

1. **Task 1: VFS layer — vnode, dentry, page cache, fd table, path walk, syscalls** - `7126bd3` (feat)
2. **Task 2: syscalls, fault_in_page, VmArea file-backed fields, QEMU VFS test** - `83d88e3` (feat)

## Files Created/Modified
- `kernel/src/fs/vnode.rs` - Vnode trait + Ext4Vnode with vnode_id/vtype/size
- `kernel/src/fs/dentry.rs` - Global BTreeMap dentry cache for path component lookup
- `kernel/src/fs/page_cache.rs` - 3-state page cache (Absent/Fetching/Cached) with probe/complete API
- `kernel/src/fs/fd_table.rs` - Per-process FdTable with Arc<OpenFile>, fork support
- `kernel/src/fs/path.rs` - Iterative path walk from root vnode through dentry + delegate
- `kernel/src/fs/syscalls.rs` - sys_open, sys_read, sys_write (stub), sys_close, sys_stat
- `kernel/src/fs/delegate.rs` - Extended with Lookup/Stat/ReadPage request types
- `kernel/src/fs/ext4.rs` - Added lookup_in_dir and read_file_at methods
- `kernel/src/mm/vm/vm_map.rs` - VmArea file-backed fields + new_file_backed constructor
- `kernel/src/mm/vm/fault.rs` - Async fault_in_page with boundary anonymization
- `kernel/src/proc/task.rs` - fd_table upgraded from Vec<()> to FdTable
- `kernel/src/proc/fork.rs` - Fork copies FdTable (Arc-shared OpenFile per POSIX)
- `kernel/src/main.rs` - VFS cache init + QEMU integration test
- `Makefile` - Added "vfs read PASS" test pattern

## Decisions
- Chose BTreeMap for dentry cache over HashMap — no_std compatible, no eviction needed for competition
- Page cache uses lock-based state transitions (not lock-free CAS) — simpler, sufficient for single-delegate serialization
- Boundary page anonymization always allocates fresh frame — page cache frames are sacred (never mutated)
- Delegate ReadPage allocates frame via frame_alloc_sync and reads file data into it
- Ext4Fs extended with read_file_at for offset-based reads (page cache granularity)

## Deviations
None — followed plan as specified.

## Next Phase Readiness
- VFS layer complete, ready for ELF loading (plan 03-05)
- fault_in_page integrates with sync_fault_handler's NeedsAsyncIO path
- VmArea file-backed fields support demand-paged PT_LOAD segments

---
*Phase: 03-exception-fixup-vfs-disk-process*
*Completed: 2026-02-23*
