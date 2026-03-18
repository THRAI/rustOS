# VM-Pmap Unification — Pmap Container API

**Date:** 2026-03-16
**Status:** Implemented
**Parent:** `00-overview`
**Phase:** A (pure refactor, no behavioral change)

## Motivation

Pmap is the hardware page-table cache for VmMap. Currently it exposes flat
free functions (`pmap_enter`, `pmap_extract`, `pmap_remove`, `pmap_protect`)
with no container semantics. Common patterns like "extract then enter" are
scattered across 4+ call sites. Range operations walk page-by-page without
skipping empty subtrees.

Model pmap as a Rust map container `VA → (PA, PteFlags)` with `get`,
`entry`, range iteration, and batched TLB shootdown.

## Safety Model

`SpinMutex<Pmap, 2>` serializes all software access. Entry types need no
additional locking — they are typed views into mutex-protected memory.

Hardware concurrency (other harts' MMU reading PTEs via TLB) is handled by
`ShootdownBatch` / `pmap_shootdown`, not by the Entry API.

RISC-V without Svadu (our config: `pte.rs:68` sets A/D upfront) guarantees
hardware never writes PTEs. `OccupiedEntry` is a stable handle. If Svadu is
enabled later, `set_perm`/`promote` must switch to CAS.

## Read-Only Lookup: `get`

Replaces `pmap_extract` and `pmap_extract_with_flags`. Takes `&self`.

```rust
pub struct PmapRef {
    pa: PhysAddr,
    flags: PteFlags,
}

impl PmapRef {
    pub fn pa(&self) -> PhysAddr;
    pub fn flags(&self) -> PteFlags;
    pub fn permits_write(&self) -> bool;
    pub fn permits_exec(&self) -> bool;
}

impl Pmap {
    pub fn get(&self, va: VirtAddr) -> Option<PmapRef>;
}
```

## Mutable Entry API: `entry`

Replaces `pmap_enter`, single-page `pmap_protect`, extract-then-enter.

```rust
pub enum PmapEntry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}

pub struct OccupiedEntry<'a> {
    pte_ptr: *mut u64,          // borrows PTE slot, NOT &mut Pmap
    va: VirtAddr,
    pa: PhysAddr,
    flags: PteFlags,
    _lifetime: PhantomData<&'a mut u64>,
}

pub struct VacantEntry<'a> {
    pmap: &'a mut Pmap,
    va: VirtAddr,
}

impl OccupiedEntry<'_> {
    pub fn va(&self) -> VirtAddr;
    pub fn pa(&self) -> PhysAddr;
    pub fn flags(&self) -> PteFlags;
    pub fn insert(&mut self, pa: PhysAddr, perm: MapPerm);  // overwrite
    pub fn promote(&mut self, new_perm: MapPerm);            // upgrade perms
    pub fn remove(self) -> ShootdownToken;                   // invalidate
    pub fn set_accessed(&mut self);
    pub fn set_dirty(&mut self);
}

impl VacantEntry<'_> {
    pub fn insert(self, pa: PhysAddr, perm: MapPerm) -> Result<(), ()>;
}

impl Pmap {
    pub fn entry(&mut self, va: VirtAddr) -> PmapEntry<'_>;

    pub fn entry_or_insert(
        &mut self, va: VirtAddr, pa: PhysAddr, perm: MapPerm,
    ) -> Result<(), ()> {
        match self.entry(va) {
            PmapEntry::Occupied(_) => Ok(()),
            PmapEntry::Vacant(v) => v.insert(pa, perm),
        }
    }
}
```

## Read-Only Range Iterator: `range`

Standard `Iterator`. Skips empty subtrees: invalid L0 entry skips 1GB,
invalid L1 entry skips 2MB.

```rust
pub struct PmapRange<'a> {
    _pmap: &'a Pmap,
    root_pa: usize,
    end_va: usize,
    stack: [(usize, usize, usize); 3],  // (table_pa, cur_idx, end_idx)
    depth: usize,
}

impl Iterator for PmapRange<'_> {
    type Item = (VirtAddr, PhysAddr, PteFlags);
}

impl Pmap {
    pub fn range(&self, start: VirtAddr, end: VirtAddr) -> PmapRange<'_>;
}
```

## Mutable Range Visitor: `for_each_in_range_mut`

Cannot implement `Iterator` (multiple `&mut` PTE borrows). Uses closure
with recursive subtree-skipping walk.

```rust
pub struct OccupiedEntryMut<'a> {
    pte_ptr: *mut u64,          // borrows PTE slot, NOT &mut Pmap
    va: VirtAddr,
    pa: PhysAddr,
    flags: PteFlags,
    _lifetime: PhantomData<&'a mut u64>,
}

impl OccupiedEntryMut<'_> {
    pub fn va(&self) -> VirtAddr;
    pub fn pa(&self) -> PhysAddr;
    pub fn flags(&self) -> PteFlags;
    pub fn remove(self) -> ShootdownToken;
    pub fn set_perm(&mut self, perm: MapPerm) -> ShootdownToken;
}

impl Pmap {
    pub fn for_each_in_range_mut<F>(
        &mut self, start: VirtAddr, end: VirtAddr, f: F,
    ) where F: FnMut(OccupiedEntryMut<'_>);
}
```

Internal walk pseudocode (recursive, parameterized by Sv39 level):

```
walk_range_mut(table_pa, start, end, level, callback):
    shift = 12 + 9 * (2 - level)
    for idx in start_idx..=end_idx:
        pte = table[idx]
        if !valid(pte): continue          // subtree skip
        if leaf(pte): callback(entry)
        else: walk_range_mut(pte_pa, clipped_start, clipped_end, level+1, cb)
```

## ShootdownBatch

Deferred TLB flush: quiesce-on-create, collect-during-mutation,
adaptive-flush-on-drop.

```rust
pub struct ShootdownToken(VirtAddr);

pub struct ShootdownBatch {
    dirty_vas: Vec<VirtAddr>,
    asid: u16,
    _quiesce: QuiesceGuard,  // remote harts parked while alive
}

impl ShootdownBatch {
    pub fn new(pmap: &Pmap) -> Self;
    pub fn add(&mut self, token: ShootdownToken);
}

impl Drop for ShootdownBatch {
    fn drop(&mut self) {
        // Adaptive local flush: per-VA if ≤ threshold, full-ASID if more.
        // _quiesce drops → remote harts flush_all and resume.
    }
}
```

## Reimplemented Operations

```rust
pub fn pmap_remove(pmap: &mut Pmap, start: VirtAddr, end: VirtAddr) {
    let mut batch = ShootdownBatch::new(pmap);
    let mut removed = 0usize;
    pmap.for_each_in_range_mut(start, end, |entry| {
        batch.add(entry.remove());
        removed += 1;
    });
    pmap.stats.resident_count -= removed;
}

pub fn pmap_protect(pmap: &mut Pmap, start: VirtAddr, end: VirtAddr, perm: MapPerm) {
    let mut batch = ShootdownBatch::new(pmap);
    pmap.for_each_in_range_mut(start, end, |mut entry| {
        batch.add(entry.set_perm(perm));
    });
}
```

## Legacy Wrappers

Existing free functions delegate to the container API during migration:

```rust
pub fn pmap_enter(...) -> Result<(), ()> { /* → entry().insert/Vacant.insert */ }
pub fn pmap_extract(...) -> Option<PhysAddr> { /* → get().map(|r| r.pa()) */ }
```

Removed after all callers migrate.

## Files

| File | Change |
|------|--------|
| `mm/pmap/mod.rs` | Add `get`, `entry`, `entry_or_insert`, `range`, `for_each_in_range_mut`. Reimplement `pmap_remove`/`pmap_protect`. |
| `mm/pmap/entry.rs` (new) | `PmapRef`, `PmapEntry`, `OccupiedEntry`, `VacantEntry`, `OccupiedEntryMut`, `PmapRange` |
| `mm/pmap/shootdown.rs` | Add `ShootdownBatch`, `ShootdownToken` |
| `mm/vm/fault.rs` | Use `entry_or_insert`, `entry().promote()` |
| `mm/vm/fault_async.rs` | Use `get`, `entry_or_insert` |
