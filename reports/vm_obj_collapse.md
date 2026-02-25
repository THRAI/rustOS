# vm_object collapse

## Context

  After fork, parent and child each get a shadow `VmObject` pointing to a shared backing object. When parent does brk shrink, it removes its PTEs but can't free backing pages (child still holds an Arc). Those pages become "phantoms" — no PTE points to them, but they're stuck in the backing object. Worse, when the child COW-faults on such a page, it copies instead of promoting in-place, because it can't tell it's the sole user.

  FreeBSD solves this with `vm_object_collapse`: when a backing object's shadow_count drops to 1, pages are renamed (moved) from backing into the sole remaining shadow. Conflicting pages (shadow already has one at that offset) are freed from backing. This eliminates phantoms lazily.

## Design

### 1. Add `shadow_count` to VmObject

```rust
pub struct VmObject {
  pages: BTreeMap<u64, OwnedPage>,
  backing: Option<Arc<RwLock<VmObject>>>,
  shadow_count: usize,   // NEW: how many shadows point to this as backing
  size: usize,
  resident_count: usize,
}
```

- `new_shadow()`: after creating the shadow, lock parent and `parent.shadow_count += 1`
- `Drop`: if self.backing is Some, lock backing and `backing.shadow_count -= 1`

### 2. `vm_object_collapse()` — Page Migration

Called when `backing.shadow_count == 1` (this object is the sole shadow). Migrates pages from backing into self:

```rust
fn collapse(&mut self):
  let backing_arc = self.backing.take()   // temporarily detach
  let mut backing = backing_arc.write()

  // Migrate pages from backing into self
  for (offset, page) in backing.pages.drain():
    if self.pages.contains_key(offset):
      // Conflict: shadow already has this page (COW copy exists)
      // Free the backing's phantom page
      frame_free(page.phys)
    else:
      // No conflict: rename page from backing to self
      self.pages.insert(offset, page)
      self.resident_count += 1

      // Adopt backing's backing (skip over the now-empty backing)
      self.backing = backing.backing.take()
      if let Some(ref grandparent) = self.backing:
        grandparent.write().shadow_count += 1  // we now shadow grandparent

        // backing will be dropped (shadow_count on grandparent was already
        // decremented when we took backing.backing, but we re-incremented above)
        // backing.shadow_count should be 0 now
      drop(backing)
      drop(backing_arc)  // Arc refcount -> 0, triggers backing's Drop
```

### 3. COW Rename Optimization

In `handle_cow_fault()`, replace the `Arc::strong_count` check with a shadow_count check:

```rust
     // Current (wrong granularity):
     let refcount = Arc::strong_count(&vma.object);
     if refcount == 1 { /* sole owner, skip copy */ }

     // New (correct — checks if backing has single shadow):
     let obj = vma.object.read();
     if let Some(backing) = obj.backing() {
         let backing_r = backing.read();
         if backing_r.shadow_count == 1 {
             // We're the sole shadow. Check if page is in backing.
             // If so, rename it to our object instead of copying.
             // ... (zero-copy promotion)
         }
     }
```

For the simple case (page is in top-level object, no backing involved), `Arc::strong_count == 1` is still valid as a fast path.

### 4. Trigger Points

1. `VmObject::drop` (mandatory): When a shadow is dropped and backing's shadow_count drops to 1, the remaining shadow should collapse on its next fault or explicitly. But we can't reach the remaining shadow from Drop — we only know the backing. So collapse is triggered lazily.

2. COW fault path (performance): Before doing a COW copy, check if `backing.shadow_count == 1`. If so, call `collapse()` first, then the page is local and no copy needed.

3. `brk` shrink (optional, deferred): After truncating, could attempt collapse. But the lazy approach (collapse on next COW fault) is simpler and sufficient.

### 5. Lock Ordering

To avoid deadlock: always lock child (shadow) before parent (backing).

- `collapse()` holds self write lock, then locks backing write lock — safe if self is always the child.
- `new_shadow()` creates shadow first, then locks parent to increment shadow_count — no deadlock (shadow isn't accessible yet).
- Drop locks backing to decrement shadow_count — safe (we're being destroyed, no one else holds our lock).

### 6. Handling backing_object_offset

FreeBSD tracks `backing_object_offset` for when a shadow covers a sub-range of the backing. Our current model uses `obj_offset` on the VMA, not on the `VmObject`. For now, we assume offset 0 (shadow covers same range as backing). This is correct for fork (full address space copy) and brk (heap VMA).

## Files to Modify

### 1. kernel/src/mm/vm/vm_object.rs — Core changes

- Add `shadow_count` field to VmObject
- Update `new()`, `new_shadow()` to manage `shadow_count`
- Add `collapse()` method
- Update Drop to decrement backing's `shadow_count`
- Add `shadow_count()` accessor

### 2. kernel/src/mm/vm/fault.rs — COW optimization

- handle_cow_fault(): attempt collapse when backing.shadow_count == 1, then rename page instead of copying

### 3. kernel/src/mm/vm/vm_map.rs — No changes needed (fork already creates shadows correctly)

## Verification

```shell
cargo build --release -p kernel --target riscv64gc-unknown-none-elf
#— compiles

cargo test --lib -p kernel-mm --target aarch64-apple-darwin
#— 96/96 host unit tests pass

make qemu-test-rv64 LOG=all
#— 21/21 integration tests pass (including fork bomb stress)
```

## ASCII Art — Collapse Lifecycle

```
  BEFORE FORK                        AFTER FORK (COW)
  ──────────                         ────────────────

  ┌─────-─────┐                  ┌──────────┐   ┌──────────┐
  │ Process P │                  │ Parent   │   │ Child    │
  │   VmArea  │                  │  shadow  │   │  shadow  │
  └─────┬─────┘                  └────┬─────┘   └────┬─────┘
        │                             │              │
        ▼                             ▼              ▼
  ┌──────────┐                  ┌──────────┐   ┌──────────┐
  │ VmObject │                  │ VmObject │   │ VmObject │
  │ F0 F1 F2 │                  │ (empty)  │   │ (empty)  │
  └──────────┘                  │ sc=0     │   │ sc=0     │
                                └────┬─────┘   └────┬─────┘
                                     │              │
                                     └──────┬───────┘
                                            ▼
                                      ┌──────────┐
                                      │ Backing  │
                                      │ F0 F1 F2 │  ← "phantom pages"
                                      │ sc=2     │     live here
                                      └──────────┘


  PARENT WRITES TO F0 (COW)               PARENT EXITS
  ─────────────────────────              ─────────────

  ┌──────────┐   ┌──────────┐            ┌──────────┐
  │ Parent   │   │ Child    │            │ Child    │
  │  shadow  │   │  shadow  │            │  shadow  │
  └────┬─────┘   └────┬─────┘            └────┬─────┘
       │              │                       │
       ▼              ▼                       ▼
  ┌──────────┐   ┌──────────┐            ┌──────────┐
  │ VmObject │   │ VmObject │            │ VmObject │
  │ F0'(copy)│   │ (empty)  │            │ (empty)  │
  └────┬─────┘   └────┬─────┘            └────┬─────┘
       │              │                       │
       └──────┬───────┘                       ▼
              ▼                          ┌──────────┐
        ┌──────────┐                     │ Backing  │
        │ Backing  │                     │ F0 F1 F2 │  ← F0 is phantom!
        │ F0 F1 F2 │                     │ sc=1     │     nobody needs it
        │ sc=2     │                     └──────────┘
        └──────────┘


  COLLAPSE (sc == 1, sole shadow)     AFTER COLLAPSE
  ───────────────────────────────     ───────────────

  ┌──────────┐                        ┌──────────┐
  │ Child    │                        │ Child    │
  │  shadow  │                        │  shadow  │
  └────┬─────┘                        └────┬─────┘
       │                                   │
       ▼                                   ▼
  ┌──────────┐                        ┌──────────┐
  │ VmObject │ ◄── migrate ──┐       │ VmObject │
  │ (empty)  │               │       │ F0 F1 F2 │  ← pages renamed
  └────┬─────┘               │       │ sc=0     │     from backing
       │                     │       └──────────┘     (zero-copy!)
       ▼                     │
  ┌──────────┐               │        Backing object is now empty
  │ Backing  │ ──────────────┘        and gets dropped. Chain shortened.
  │ F0 F1 F2 │
  │ sc=1     │
  └──────────┘


  COW FAULT WITH CONFLICT             RESULT
  ───────────────────────             ──────

  ┌──────────┐                        ┌──────────┐
  │ Child    │                        │ Child    │
  └────┬─────┘                        └────┬─────┘
       ▼                                   ▼
  ┌──────────┐                        ┌──────────┐
  │ VmObject │                        │ VmObject │
  │ F0'(cow) │ ◄── conflict!         │ F0'  F1  │  F0' kept (child's copy)
  └────┬─────┘     F0 in backing     └──────────┘  F0  freed (phantom!)
       ▼           already has F0'                  F1  renamed from backing
  ┌──────────┐
  │ Backing  │
  │ F0  F1   │
  └──────────┘


  GRANDPARENT ADOPTION                RESULT
  ────────────────────                ──────

  ┌──────────┐                        ┌──────────┐
  │ Child    │                        │ Child    │
  └────┬─────┘                        └────┬─────┘
       ▼                                   ▼
  ┌──────────┐                        ┌──────────┐
  │ Shadow   │                        │ Shadow   │
  │ (empty)  │                        │ F1       │  ← migrated from parent
  └────┬─────┘                        └────┬─────┘
       ▼                                   │
  ┌──────────┐                             ▼        parent dropped,
  │ Parent   │                        ┌──────────┐  shadow relationship
  │ F1       │                        │Grandpar. │  transferred (not
  │ sc=1     │                        │ F0       │  double-counted)
  └────┬─────┘                        │ sc=1     │
       ▼                              └──────────┘
  ┌──────────┐
  │Grandpar. │
  │ F0       │
  │ sc=1     │
  └──────────┘
```

## Fork Bomb Stress Test

```
                              ┌──────────┐
                              │   Root   │
                              │ F0..F3   │
                              │ sc=32    │
                              └────┬─────┘
                                   │
            ┌──────┬──────┬────────┼────────┬──────┬──────┐
            ▼      ▼      ▼        ▼        ▼      ▼      ▼
         ┌─────┐┌─────┐┌─────┐ ┌─────┐ ┌─────┐┌─────┐┌─────┐
         │ C0  ││ C1  ││ C2  │ │ ... │ │ C29 ││ C30 ││ C31 │  32 children
         │sc=1 ││sc=1 ││sc=1 │ │     │ │sc=1 ││sc=1 ││sc=1 │
         └──┬──┘└──┬──┘└──┬──┘ └─────┘ └──┬──┘└──┬──┘└──┬──┘
            ▼      ▼      ▼                ▼      ▼      ▼
         ┌─────┐┌─────┐┌─────┐          ┌─────┐┌─────┐┌─────┐
         │ G0  ││ G1  ││ G2  │   ...    │ G29 ││ G30 ││ G31 │  32 grandchildren
         └─────┘└─────┘└─────┘          └─────┘└─────┘└─────┘

  Phase 1: Create 32 children (shadows of root)
  Phase 2: Each child forks a grandchild
  Phase 3: Drop all children — grandchildren keep them alive via Arc
  Phase 4: Drop grandchildren one by one — verify page visibility at each step
  Phase 5: Verify root intact, shadow_count == 0, all pages present

  Result: No panics, no leaks, no corruption.
```
