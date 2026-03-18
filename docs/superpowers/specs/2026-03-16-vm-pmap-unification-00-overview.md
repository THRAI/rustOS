# VM-Pmap Unification — Overview

**Date:** 2026-03-16
**Status:** Implemented

## Problem

Pmap (hardware page tables) and VmMap (virtual address space description)
modifications are inconsistently coupled, creating three categories of defect:

1. **Pmap-only mappings with no VMA.** The sigcode trampoline page is mapped
   via raw `pmap_enter` (`signal.rs:718`) with no `VmMapEntry`. It is
   invisible to `munmap`, `mprotect`, and `VmMap::clear()`.

2. **VMA metadata not reflecting pmap state.** During COW fork,
   `pmap_protect` downgrades parent PTEs to RO, but the VMA's `protection`
   field stays RW. `EntryFlags::COW | NEEDS_COPY` (defined at
   `entry.rs:16-17`) are never set. The semantic gap is resolved implicitly
   by the fault handler.

3. **Scattered pmap manipulation outside VmMap.** Fork COW setup
   (`fork.rs:124-258`), sigcode mapping (`fork.rs:43-60`), and fault
   handlers call pmap free functions directly. Common patterns like
   "extract then enter" appear 4+ times with no abstraction.

Additionally, `sys_clone` (`process.rs:52-91`) is a thin wrapper around a
single `fork()` function that ignores `CLONE_VM`, `CLONE_VFORK`,
`CLONE_FILES`, `CLONE_SIGHAND`, and rejects `CLONE_THREAD`.

## Architecture

Four layers, each with a single responsibility:

```
┌──────────────────────────────────────────────────────────────┐
│  Callers (sys_mmap, do_execve, map_sigcode)                  │
│    Build VmObject, choose VmMapping variant                  │
│    Call: VmMap.map(start, end, perm, VmMapping::*)            │
├──────────────────────────────────────────────────────────────┤
│  VmMap Builder + Business Methods                            │
│    map()           — construct entry from VmMapping enum      │
│    cow_fork_into() — COW fork (compose accountant methods)    │
│    share_into()    — shared address space (CLONE_VM)          │
│    map_sigcode()   — sigcode VMA via VmMapping::Shared        │
├──────────────────────────────────────────────────────────────┤
│  VmMap Accountant (CRUD + pmap bind + invariant enforcement) │
│    insert_entry, remove_range, protect_range                 │
│    grow_heap, shrink_heap, clear                             │
│    rebind_store, set_cow_state                               │
├──────────────────────────────────────────────────────────────┤
│  Pmap Container (hardware cache)                             │
│    get, entry → Occupied | Vacant                            │
│    range (read-only iterator, subtree-skipping)              │
│    for_each_in_range_mut (mutable visitor)                   │
│    ShootdownBatch (quiesce-on-create, flush-on-drop)         │
└──────────────────────────────────────────────────────────────┘
```

**Data flow:** Builder chooses `VmMapping` variant (carries VmObject) →
`VmMap::map()` stores it directly in `VmMapEntry` → clone reads
`entry.inheritance()` (derived from variant discriminant) to decide
COW / share / skip.

## Sub-Specs

| Doc | Scope | Dependencies |
|-----|-------|-------------|
| `01-pmap-container` | Pmap as Rust container: get/entry/range/for_each_in_range_mut/ShootdownBatch | None |
| `02-vmmapping-entry` | VmMapping enum, VmMapEntry restructure, CowState | None |
| `03-vmmap-builder-accountant` | VmMap::map() builder, accountant methods, sigcode VMA | 01 + 02 |
| `04-unified-clone` | do_clone, cow_fork_into, share_into, Task Arc-wrapping | 01 + 02 + 03 |

## Phasing

| Phase | Scope | Test |
|-------|-------|------|
| **A** | Pmap container API (01). Re-implement `pmap_remove`/`pmap_protect` on top. Legacy wrappers for existing callers. | `make agent-test` (no behavioral change) |
| **B** | VmMapping enum + VmMapEntry restructure (02). VmMap::map() builder + accountant methods (03). Migrate sys_mmap, exec, grow_heap. Sigcode VMA. | `make python-test-rv64` |
| **C** | cow_fork_into, share_into, do_clone, CloneFlags, Task Arc-wrapping (04). CLONE_VM, CLONE_FILES, CLONE_VFORK, CLONE_SIGHAND. | `make python-test-rv64` + vfork workloads |
| **D** | CLONE_THREAD + thread groups (future, not specced here). | pthread tests |

Each phase is independently shippable. Phase A is a pure refactor.

## Files Modified

| File | Phase |
|------|-------|
| `mm/pmap/mod.rs` | A |
| `mm/pmap/entry.rs` (new) | A |
| `mm/pmap/shootdown.rs` | A |
| `mm/vm/fault.rs` | A, B |
| `mm/vm/fault_async.rs` | A, B |
| `mm/vm/map/entry.rs` | B |
| `mm/vm/map/mod.rs` | B, C |
| `syscall/memory.rs` | B |
| `proc/exec.rs` | B |
| `proc/signal.rs` | B |
| `proc/fork.rs` → `proc/clone.rs` | C |
| `proc/task.rs` | C |
| `syscall/process.rs` | C |
