# VM-Pmap Unification — VmMapping Enum and VmMapEntry

**Date:** 2026-03-16
**Status:** Implemented
**Parent:** `00-overview`
**Phase:** B

## Motivation

`VmMapEntry` currently stores three overlapping descriptors for region
semantics: `BackingStore` (Object/SubMap/Guard), `EntryFlags` (COW,
NEEDS_COPY, HEAP, GROWS_DOWN), and `VmInherit` (Share/Copy/None). These
are independently settable, creating invalid combinations that the type
system cannot prevent (e.g., a Guard entry with `VmInherit::Share`).

Replace all three with a single `VmMapping` enum whose variant
determines backing store, inheritance, and creation-time flags. Add a
separate `CowState` field for mutable lifecycle state set by fork.

## VmMapping Enum

Passed by builders (sys_mmap, exec) into `VmMap::map()`. Stored directly
in `VmMapEntry`. The variant carries the VmObject and offset.

```rust
pub enum VmMapping {
    /// Anonymous private (mmap MAP_ANON|MAP_PRIVATE, exec stack).
    AnonPrivate {
        object: Arc<LeveledRwLock<VmObject, 3>>,
    },
    /// File-backed private (mmap MAP_PRIVATE on file, exec PT_LOAD).
    /// Shadow of vnode object; writes are process-private.
    FilePrivate {
        object: Arc<LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    /// File-backed shared (mmap MAP_SHARED). Writes go to vnode.
    FileShared {
        object: Arc<LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    /// Heap region managed by brk(2).
    Heap {
        object: Arc<LeveledRwLock<VmObject, 3>>,
    },
    /// Shared page (sigcode trampoline, future POSIX shm).
    Shared {
        object: Arc<LeveledRwLock<VmObject, 3>>,
    },
    /// Guard page. No backing; faults always fatal.
    Guard,
}
```

## Derived Properties

Inheritance and object access are derived from the variant, never stored
as independent fields:

```rust
impl VmMapping {
    /// How fork/clone treats this entry. Not stored — derived.
    pub fn inheritance(&self) -> VmInherit {
        match self {
            Self::AnonPrivate { .. }
            | Self::FilePrivate { .. }
            | Self::Heap { .. } => VmInherit::Copy,
            Self::FileShared { .. }
            | Self::Shared { .. } => VmInherit::Share,
            Self::Guard => VmInherit::None,
        }
    }

    /// The backing object, if any.
    pub fn object(&self) -> Option<&Arc<LeveledRwLock<VmObject, 3>>> {
        match self {
            Self::AnonPrivate { object }
            | Self::FilePrivate { object, .. }
            | Self::FileShared { object, .. }
            | Self::Heap { object }
            | Self::Shared { object } => Some(object),
            Self::Guard => None,
        }
    }

    /// The file offset (0 for non-file-backed).
    pub fn offset(&self) -> u64 {
        match self {
            Self::FilePrivate { offset, .. }
            | Self::FileShared { offset, .. } => *offset,
            _ => 0,
        }
    }
}
```

## CowState

Mutable lifecycle state, set by fork. Orthogonal to role within the
Copy-inheritable subset. Share and None roles can never enter
CopyOnWrite.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CowState {
    None,
    CopyOnWrite,
}
```

## VmMapEntry Restructured

```rust
pub(crate) struct VmMapEntry {
    // Splay tree internals
    range: VirtAddrRange,
    max_free: u64,
    splay_node: Node,

    // The mapping — carries object, offset, role (from builder)
    pub(crate) mapping: VmMapping,

    // Current protection (mutable by mprotect)
    pub(crate) protection: MapPerm,
    pub(crate) max_protection: MapPerm,

    // Lifecycle state (mutable by fork)
    pub(crate) cow_state: CowState,
}
```

## Derived Entry Methods

```rust
impl VmMapEntry {
    pub fn inheritance(&self) -> VmInherit {
        self.mapping.inheritance()
    }

    pub fn is_cow(&self) -> bool {
        self.cow_state == CowState::CopyOnWrite
    }

    /// Compute VmObject + page offset for a fault VA.
    pub fn extract_object_offset(
        &self, fault_va: VirtAddr,
    ) -> Option<(Arc<LeveledRwLock<VmObject, 3>>, VObjIndex)> {
        let object = self.mapping.object()?.clone();
        let base_offset = self.mapping.offset();
        let offset_bytes = base_offset + (fault_va.as_usize() as u64 - self.start());
        Some((object, VObjIndex::from_bytes_floor(offset_bytes as usize)))
    }

    /// Clone for split (protect_range, remove_range).
    /// Arc::clone on object, preserve cow_state, adjust offset.
    pub fn clone_for_split(&self, split_addr: u64) -> Self { ... }

    /// Can two adjacent entries merge?
    pub fn is_mergeable_with(&self, next: &VmMapEntry) -> bool {
        // Same variant discriminant, Arc::ptr_eq on object,
        // contiguous offset, same protection, same cow_state.
    }
}
```

## What Disappeared

| Old type / field | Replacement |
|-----------------|-------------|
| `BackingStore` enum | Absorbed into `VmMapping` variants |
| `EntryFlags` bitfield | `HEAP` → `VmMapping::Heap`; `COW\|NEEDS_COPY` → `CowState` |
| `VmInherit` field | Derived from `VmMapping::inheritance()` |
| `store: BackingStore` | `mapping: VmMapping` |
| `flags: EntryFlags` | Gone (role in variant, lifecycle in `CowState`) |
| `inheritance: VmInherit` | Gone (derived) |
| `pub VmMapEntry::new()` | `pub(crate)`, callers use `VmMap::map()` |

## Migration

| Old pattern | New pattern |
|------------|------------|
| `BackingStore::Object { object, offset }` | `VmMapping::AnonPrivate { object }` or `FilePrivate { object, offset }` etc |
| `BackingStore::Guard` | `VmMapping::Guard` |
| `EntryFlags::HEAP` | `VmMapping::Heap { .. }` |
| `EntryFlags::COW \| NEEDS_COPY` | `entry.cow_state = CowState::CopyOnWrite` |
| `vma.inheritance == VmInherit::Share` | `vma.inheritance() == VmInherit::Share` (method call) |
| `vma.store = new_store` | `VmMap::rebind_store(va, new_object)` |

## Files

| File | Change |
|------|--------|
| `mm/vm/map/entry.rs` | Replace struct fields. Add `VmMapping`, `CowState`. Remove `BackingStore`, `EntryFlags`, `VmInherit` field. |
| `mm/vm/map/mod.rs` | Update all methods that match on `BackingStore` or read `flags`/`inheritance`. |
| `mm/vm/fault.rs` | Use `entry.extract_object_offset()`, `entry.is_cow()`. |
| `mm/vm/fault_async.rs` | Same. |
