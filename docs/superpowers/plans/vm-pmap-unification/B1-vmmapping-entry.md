# VM-Pmap Unification — Phase B1: VmMapping Enum + VmMapEntry Restructure

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `BackingStore`, `EntryFlags`, and `VmInherit` field with `VmMapping` enum and `CowState`. Restructure `VmMapEntry` to store `VmMapping` directly.

**Architecture:** `VmMapping` enum has 6 variants (AnonPrivate, FilePrivate, FileShared, Heap, Shared, Guard), each carrying its VmObject+offset. `VmMapEntry` stores `mapping: VmMapping` + `cow_state: CowState` + `protection` + `max_protection`. Inheritance is derived via `mapping.inheritance()`. All callers are migrated in one pass.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-02-vmmapping-entry.md`

**Depends on:** None (independent of Phase A)

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/mm/vm/map/entry.rs` | Modify | Replace struct fields: remove `store`, `flags`, `inheritance`. Add `mapping: VmMapping`, `cow_state: CowState`. Add `VmMapping` enum, `CowState` enum. Update all methods. |
| `kernel/src/mm/vm/mod.rs` | Modify | Update re-exports: remove `BackingStore`, `EntryFlags`. Add `VmMapping`, `CowState`. |
| `kernel/src/mm/vm/map/mod.rs` | Modify | Update all methods that match on `BackingStore` or read `flags`/`store`. |
| `kernel/src/mm/vm/fault.rs` | Modify | Update `extract_object_offset` calls, `BackingStore` matches. |
| `kernel/src/mm/vm/fault_async.rs` | Modify | Same. |
| `kernel/src/syscall/memory.rs` | Modify | Replace `VmMapEntry::new(... BackingStore::Object ..., EntryFlags::empty() ...)` with `VmMapping` variant. |
| `kernel/src/proc/exec.rs` | Modify | Same for all `VmMapEntry::new` calls. |
| `kernel/src/proc/fork.rs` | Modify | Replace `BackingStore` matches, `EntryFlags` reads, `vma.store` accesses. |

---

### Task 1: Define VmMapping and CowState

**Files:**
- Modify: `kernel/src/mm/vm/map/entry.rs`

- [ ] **Step 1: Add VmMapping enum and CowState enum (at top of file, before VmMapEntry)**

```rust
use alloc::sync::Arc;
use crate::hal_common::LeveledRwLock;
use super::VmObject;

/// Region kind + backing store. Determines inheritance behavior for fork.
#[derive(Clone)]
pub enum VmMapping {
    AnonPrivate { object: Arc<LeveledRwLock<VmObject, 3>> },
    FilePrivate { object: Arc<LeveledRwLock<VmObject, 3>>, offset: u64 },
    FileShared  { object: Arc<LeveledRwLock<VmObject, 3>>, offset: u64 },
    Heap        { object: Arc<LeveledRwLock<VmObject, 3>> },
    Shared      { object: Arc<LeveledRwLock<VmObject, 3>> },
    Guard,
}

/// Fork inheritance, derived from VmMapping variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmInherit { Copy, Share, None }

impl VmMapping {
    pub fn inheritance(&self) -> VmInherit {
        match self {
            Self::AnonPrivate{..} | Self::FilePrivate{..} | Self::Heap{..} => VmInherit::Copy,
            Self::FileShared{..} | Self::Shared{..} => VmInherit::Share,
            Self::Guard => VmInherit::None,
        }
    }
    pub fn object(&self) -> Option<&Arc<LeveledRwLock<VmObject, 3>>> {
        match self {
            Self::AnonPrivate{object} | Self::FilePrivate{object,..}
            | Self::FileShared{object,..} | Self::Heap{object}
            | Self::Shared{object} => Some(object),
            Self::Guard => None,
        }
    }
    pub fn offset(&self) -> u64 {
        match self {
            Self::FilePrivate{offset,..} | Self::FileShared{offset,..} => *offset,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CowState { None, CopyOnWrite }
```

- [ ] **Step 2: Build** (will fail — VmMapEntry not yet updated)
- [ ] **Step 3: Commit** — `feat(vm): add VmMapping enum and CowState`

### Task 2: Restructure VmMapEntry

**Files:**
- Modify: `kernel/src/mm/vm/map/entry.rs`

- [ ] **Step 1: Replace struct fields**

Remove `store: BackingStore`, `flags: EntryFlags`, `inheritance: VmInherit`.
Add `mapping: VmMapping`, `cow_state: CowState`.

```rust
pub struct VmMapEntry {
    pub(crate) range: VirtAddrRange,
    pub(crate) max_free: u64,
    pub(crate) splay_node: Node,
    pub(crate) mapping: VmMapping,
    pub(crate) protection: MapPerm,
    pub(crate) max_protection: MapPerm,
    pub(crate) cow_state: CowState,
}
```

- [ ] **Step 2: Update `VmMapEntry::new()` to take VmMapping**

```rust
impl VmMapEntry {
    pub fn new(start: u64, end: u64, mapping: VmMapping, protection: MapPerm) -> Self {
        Self {
            range: VirtAddrRange::from_raw(start, end),
            max_free: 0,
            splay_node: Node::new(),
            mapping,
            protection,
            max_protection: protection,
            cow_state: CowState::None,
        }
    }
}
```

- [ ] **Step 3: Update derived methods**

Update `extract_object_offset`, `clone_for_split`, `is_mergeable_with`, `size`, `start`, `end`, `set_bounds` to use `self.mapping` instead of `self.store`.

- [ ] **Step 4: Fix all compile errors across the codebase**

This is the largest step. Every file that constructs `VmMapEntry` or reads `store`/`flags`/`inheritance` needs updating. Key migrations:

| Old pattern | New pattern |
|------------|------------|
| `VmMapEntry::new(s,e, BackingStore::Object{object,offset}, EntryFlags::empty(), perm)` | `VmMapEntry::new(s,e, VmMapping::AnonPrivate{object}, perm)` (or FilePrivate etc) |
| `vma.store` | `vma.mapping` |
| `vma.flags.contains(EntryFlags::HEAP)` | `matches!(vma.mapping, VmMapping::Heap{..})` |
| `vma.flags.contains(EntryFlags::COW)` | `vma.cow_state == CowState::CopyOnWrite` |
| `BackingStore::Object{object,offset}` pattern | `VmMapping::AnonPrivate{object}` etc |
| `BackingStore::Guard` | `VmMapping::Guard` |

- [ ] **Step 5: Build** — `make kernel-rv64`
- [ ] **Step 6: Smoke test** — `make agent-test`
- [ ] **Step 7: Commit** — `refactor(vm): restructure VmMapEntry to use VmMapping enum`

### Task 3: Delete old types

**Files:**
- Modify: `kernel/src/mm/vm/map/entry.rs` — remove `BackingStore`, `EntryFlags` (if no remaining usages)
- Modify: `kernel/src/mm/vm/mod.rs` — update re-exports

- [ ] **Step 1: Remove `BackingStore` and `EntryFlags` definitions (if all callers migrated)**
- [ ] **Step 2: Update re-exports in `mm/vm/mod.rs`**
- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `refactor(vm): remove BackingStore and EntryFlags`

### Task 4: Full integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Smoke test** — `make agent-test`
- [ ] **Step 3: Full integration** — `make python-test-rv64`
