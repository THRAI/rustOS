# VM-Pmap Unification — VmMap Builder and Accountant Methods

**Date:** 2026-03-16
**Status:** Implemented
**Parent:** `00-overview`
**Phase:** B
**Depends on:** `01-pmap-container`, `02-vmmapping-entry`

## Motivation

Currently `VmMapEntry` is constructed externally by sys_mmap, exec, and
fork, then handed to `insert_entry`. This leaves validation, flag
derivation, and bookkeeping scattered across callers. Some callers also
mutate entry fields directly via `lookup_mut` (e.g., `fork.rs:206`
replaces `vma.store`, `exec.rs:724` mutates `vma.protection`) without
bumping the timestamp.

VmMap should expose a single builder function `map()` for region
creation, and accountant methods for all field mutations. External code
never constructs or mutates `VmMapEntry` directly.

## Builder: `VmMap::map()`

The single entry point for mapping a region. Receives the `VmMapping`
enum (which carries the VmObject), derives all entry fields, validates,
inserts, and maintains bookkeeping.

```rust
impl VmMap {
    /// Map a region into the address space.
    ///
    /// Derives BackingStore, flags, and inheritance from the VmMapping
    /// variant. Validates alignment and overlap. Bumps timestamp.
    /// Does NOT touch pmap (lazy fault model).
    pub fn map(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        perm: MapPerm,
        mapping: VmMapping,
    ) -> Result<(), Errno>;
}
```

Internally:
1. Validate page alignment, `start < end`
2. Check no overlap with existing entries
3. Construct `VmMapEntry` (pub(crate)) with `cow_state: CowState::None`
4. Insert into splay tree
5. Update `size`, `nentries`, `timestamp`

## How Callers Migrate

### sys_mmap (`memory.rs`)

```rust
// Before:
let vma = VmMapEntry::new(base, end,
    BackingStore::Object { object, offset }, EntryFlags::empty(), perm);
vm.insert_entry(vma)?;

// After:
let mapping = if map_anon {
    VmMapping::AnonPrivate { object: VmObject::new_anon(len) }
} else if map_private {
    VmMapping::FilePrivate { object: build_file_shadow(vnode), offset }
} else {
    VmMapping::FileShared { object: vnode_object(vnode), offset }
};
vm.map(VirtAddr(base), VirtAddr(base + len), perm, mapping)?;
```

### exec PT_LOAD (`exec.rs`)

```rust
// Before:
let vma = VmMapEntry::new(va_start, va_end,
    BackingStore::Object { object: vnode_obj, offset }, EntryFlags::empty(), prot);
insert_or_merge_file_vma(&mut new_vm, vma)?;

// After:
new_vm.map_or_merge(
    VirtAddr(va_start), VirtAddr(va_end), prot,
    VmMapping::FilePrivate { object: vnode_obj, offset },
)?;
```

`map_or_merge` is a variant of `map` for exec: if an entry at the exact
same `[start, end)` with the same offset already exists, merge
permissions (`|=`). Otherwise delegate to `map`. This absorbs the
`insert_or_merge_file_vma` function.

### exec stack

```rust
new_vm.map(
    VirtAddr(stack_bottom), VirtAddr(USER_STACK_TOP),
    map_perm!(R, W, U),
    VmMapping::AnonPrivate { object: VmObject::new_anon(STACK_SIZE) },
)?;
```

### exec eager stack page

After calling `map()`, exec eagerly maps the top stack page into pmap:

```rust
// After map(), enter the top stack page via pmap container API
let mut pmap = new_pmap_arc.lock();
pmap.entry_or_insert(VirtAddr(stack_page_va), phys, map_perm!(R, W, U))?;
```

This is a permitted direct pmap call (constructing a new address space
that isn't live yet).

### grow_heap

```rust
// Internally uses VmMapping::Heap:
let mapping = VmMapping::Heap { object: VmObject::new_anon(grow_len) };
self.map(VirtAddr(old_brk), VirtAddr(new_brk), map_perm!(R, W, U), mapping)?;
```

Heap extension (widening an existing Heap entry) stays as-is: it calls
`set_bounds` + resizes the VmObject.

## Sigcode VMA: `map_sigcode()`

```rust
static SIGCODE_OBJ: Once<Arc<LeveledRwLock<VmObject, 3>>> = Once::new();

pub fn init_sigcode_object() {
    let obj = VmObject::new_anon(PAGE_SIZE);
    // Allocate page, copy sigcode assembly, insert into object.
    SIGCODE_OBJ.call_once(|| obj);
}

impl VmMap {
    pub fn map_sigcode(&mut self) {
        self.map(
            VirtAddr(SIGCODE_VA),
            VirtAddr(SIGCODE_VA + PAGE_SIZE),
            map_perm!(R, X, U),
            VmMapping::Shared {
                object: SIGCODE_OBJ.get().unwrap().clone(),
            },
        ).unwrap();
        // Optionally eagerly enter into pmap:
        let pa = SIGCODE_OBJ.get().unwrap().read()
            .lookup_page(VObjIndex::new(0)).unwrap();
        let mut pmap = self.pmap.lock();
        pmap.entry_or_insert(VirtAddr(SIGCODE_VA), pa, map_perm!(R, X, U)).ok();
    }
}
```

Replaces `map_sigcode_page(&mut pmap)` at `signal.rs:703-718`.

## Accountant Methods

### Existing (preserved, use pmap container API internally)

| Method | Operation | Pmap bind |
|--------|-----------|-----------|
| `remove_range(start, end)` | D: remove/split VMAs | `pmap_remove` (via `for_each_in_range_mut` + `ShootdownBatch`) |
| `remove_entry_containing(va)` | D: remove single | `pmap_remove` |
| `protect_range(start, end, perm)` | U: split + update protection | `pmap_protect` (via `for_each_in_range_mut` + `ShootdownBatch`) |
| `grow_heap(old, new)` | C: extend or create heap | none (lazy) |
| `shrink_heap(old, new)` | D: shrink or remove heap | `pmap_remove` |
| `clear()` | D: all | `pmap_remove` all |

### New

```rust
impl VmMap {
    /// Replace the VmObject inside a VmMapping variant.
    /// The variant discriminant (AnonPrivate, FilePrivate, etc) is preserved.
    /// Bumps timestamp. Used by cow_fork_into to rebind parent stores
    /// to COW shadow objects.
    pub fn rebind_store(
        &mut self, va: u64,
        new_object: Arc<LeveledRwLock<VmObject, 3>>,
    ) -> Result<(), Errno>;

    /// Set the COW lifecycle state on an entry.
    /// Bumps timestamp. Validates that the entry's inheritance is Copy.
    pub fn set_cow_state(
        &mut self, va: u64, state: CowState,
    ) -> Result<(), Errno>;

    /// map() variant for exec: merge permissions if an identical entry
    /// exists at the same [start, end) with the same file offset.
    /// Absorbs insert_or_merge_file_vma.
    pub fn map_or_merge(
        &mut self,
        start: VirtAddr, end: VirtAddr,
        perm: MapPerm, mapping: VmMapping,
    ) -> Result<(), Errno>;
}
```

### `rebind_store` implementation

```rust
pub fn rebind_store(&mut self, va: u64, new_object: ...) -> Result<(), Errno> {
    let vma = self.lookup_mut(va).ok_or(Errno::Esrch)?;
    match &mut vma.mapping {
        VmMapping::AnonPrivate { object } => *object = new_object,
        VmMapping::FilePrivate { object, .. } => *object = new_object,
        VmMapping::Heap { object } => *object = new_object,
        _ => return Err(Errno::Einval), // Share/Guard can't be rebound
    }
    self.timestamp.fetch_add(1, Ordering::SeqCst);
    Ok(())
}
```

### `set_cow_state` implementation

```rust
pub fn set_cow_state(&mut self, va: u64, state: CowState) -> Result<(), Errno> {
    let vma = self.lookup_mut(va).ok_or(Errno::Esrch)?;
    if state == CowState::CopyOnWrite
        && vma.inheritance() != VmInherit::Copy
    {
        return Err(Errno::Einval); // only Copy-inheritable can be COW
    }
    vma.cow_state = state;
    self.timestamp.fetch_add(1, Ordering::SeqCst);
    Ok(())
}
```

## Visibility Changes

| Symbol | Before | After |
|--------|--------|-------|
| `VmMapEntry::new()` | `pub` | `pub(crate)` |
| `VmMapEntry` struct | `pub` | `pub(crate)` |
| `insert_entry()` | `pub` | `pub(crate)` (callers use `map()`) |
| `VmMapping` enum | — | `pub` (callers construct this) |

## Files

| File | Change |
|------|--------|
| `mm/vm/map/mod.rs` | Add `map()`, `map_or_merge()`, `map_sigcode()`, `rebind_store()`, `set_cow_state()`. Restrict `insert_entry` to `pub(crate)`. |
| `mm/vm/map/entry.rs` | Restrict `VmMapEntry::new` to `pub(crate)`. |
| `syscall/memory.rs` | sys_mmap: construct `VmMapping`, call `vm.map()`. |
| `proc/exec.rs` | Replace `VmMapEntry::new` + `insert_or_merge_file_vma` with `vm.map()` / `vm.map_or_merge()`. |
| `proc/signal.rs` | Add `init_sigcode_object()`, `SIGCODE_OBJ`. Remove `map_sigcode_page()`. |
