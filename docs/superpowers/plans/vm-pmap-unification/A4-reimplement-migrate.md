# VM-Pmap Unification — Phase A4: Reimplement pmap_remove/pmap_protect + Migrate Fault Handlers

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reimplement `pmap_remove` and `pmap_protect` on the container API (`for_each_in_range_mut` + `ShootdownBatch`). Migrate fault handlers from `pmap_extract`/`pmap_enter` to `get()`/`entry()`/`entry_or_insert()`. Add legacy wrappers for `pmap_extract`.

**Architecture:** `pmap_remove` and `pmap_protect` become thin wrappers that create a `ShootdownBatch` and call `for_each_in_range_mut`. Fault handlers use `entry_or_insert` for cache fill and `entry().promote()` for COW. `pmap_extract`/`pmap_extract_with_flags` delegate to `Pmap::get()`.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-01-pmap-container.md`

**Depends on:** A1 (container types), A3 (ShootdownBatch, for_each_in_range_mut)

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/mm/pmap/mod.rs` | Modify | Rewrite `pmap_remove`, `pmap_protect`. Rewrite `pmap_extract`, `pmap_extract_with_flags` as wrappers over `get()`. |
| `kernel/src/mm/vm/fault.rs` | Modify | Replace `pmap_enter` calls with `entry_or_insert`/`entry().promote()` |
| `kernel/src/mm/vm/fault_async.rs` | Modify | Replace `pmap_extract`+`pmap_enter` patterns with `get()`/`entry_or_insert()` |

---

### Task 1: Rewrite pmap_remove on container API

**Files:**
- Modify: `kernel/src/mm/pmap/mod.rs:326-362`

- [ ] **Step 1: Replace page-by-page walk with for_each_in_range_mut + ShootdownBatch**

```rust
pub fn pmap_remove(pmap: &mut Pmap, va_start: VirtAddr, va_end: VirtAddr) {
    #[cfg(target_arch = "riscv64")]
    let mut batch = shootdown::ShootdownBatch::new(&pmap.active, pmap.asid);
    #[cfg(not(target_arch = "riscv64"))]
    let mut batch = shootdown::ShootdownBatch::new(&[], 0);

    pmap.for_each_in_range_mut(va_start, va_end, |entry| {
        let was_wired = entry.flags().contains(PteFlags::SW_WIRED);
        let token = entry.remove();
        batch.add(token);
        pmap.stats.resident_count = pmap.stats.resident_count.saturating_sub(1);
        if was_wired {
            pmap.stats.wired_count = pmap.stats.wired_count.saturating_sub(1);
        }
    });
    // batch drops → local flush + release remote harts
}
```

Note: There is a borrow conflict between `pmap.for_each_in_range_mut` borrowing `&mut self` and accessing `pmap.stats` inside the closure. The implementer should track removal/wired counts as local variables and apply them after the loop:

```rust
    let mut removed = 0usize;
    let mut unwired = 0usize;
    pmap.for_each_in_range_mut(va_start, va_end, |entry| {
        if entry.flags().contains(PteFlags::SW_WIRED) { unwired += 1; }
        batch.add(entry.remove());
        removed += 1;
    });
    pmap.stats.resident_count = pmap.stats.resident_count.saturating_sub(removed);
    pmap.stats.wired_count = pmap.stats.wired_count.saturating_sub(unwired);
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Smoke test** — `make agent-test`
- [ ] **Step 4: Commit** — `refactor(pmap): rewrite pmap_remove on container API`

### Task 2: Rewrite pmap_protect on container API

**Files:**
- Modify: `kernel/src/mm/pmap/mod.rs:371-407`

- [ ] **Step 1: Replace page-by-page walk with for_each_in_range_mut + ShootdownBatch**

```rust
pub fn pmap_protect(pmap: &mut Pmap, va_start: VirtAddr, va_end: VirtAddr, prot: MapPerm) {
    #[cfg(target_arch = "riscv64")]
    let mut batch = shootdown::ShootdownBatch::new(&pmap.active, pmap.asid);
    #[cfg(not(target_arch = "riscv64"))]
    let mut batch = shootdown::ShootdownBatch::new(&[], 0);

    pmap.for_each_in_range_mut(va_start, va_end, |mut entry| {
        batch.add(entry.set_perm(prot));
    });
    // batch drops → local flush + release remote harts
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Smoke test** — `make agent-test`
- [ ] **Step 4: Commit** — `refactor(pmap): rewrite pmap_protect on container API`

### Task 3: Rewrite pmap_extract/pmap_extract_with_flags as get() wrappers

**Files:**
- Modify: `kernel/src/mm/pmap/mod.rs:410-438`

- [ ] **Step 1: Rewrite both functions to delegate to `get()`**

```rust
pub fn pmap_extract(pmap: &Pmap, va: VirtAddr) -> Option<PhysAddr> {
    pmap.get(va).map(|r| r.pa())
}

pub fn pmap_extract_with_flags(pmap: &Pmap, va: VirtAddr) -> Option<(PhysAddr, PteFlags)> {
    pmap.get(va).map(|r| (r.pa(), r.flags()))
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `refactor(pmap): rewrite pmap_extract as get() wrapper`

### Task 4: Migrate sync fault handler to container API

**Files:**
- Modify: `kernel/src/mm/vm/fault.rs`

- [ ] **Step 1: Replace `pmap_enter` calls in `handle_anonymous_fault`**

Find the pattern:
```rust
if pmap_enter(pmap, fault_va_aligned, phys, vma.protection, false).is_err() { ... }
```
Replace with:
```rust
pmap.entry_or_insert(fault_va_aligned, phys, vma.protection)
    .map_err(|_| kerr!(...))?;
```

- [ ] **Step 2: Replace COW promote path**

Find `pmap_protect(&mut pmap, fault_va_aligned, ...)` in the COW fast path and replace with:
```rust
if let crate::mm::pmap::container::PmapEntry::Occupied(mut e) = pmap.entry(fault_va_aligned) {
    e.promote(vma.protection);
}
```

- [ ] **Step 3: Replace COW full-copy `pmap_enter`**

Find `pmap_enter(pmap, fault_va_aligned, new_frame_phys, ...)` and replace with:
```rust
match pmap.entry(fault_va_aligned) {
    crate::mm::pmap::container::PmapEntry::Occupied(mut e) => e.insert(new_frame_phys, vma.protection),
    crate::mm::pmap::container::PmapEntry::Vacant(v) => v.insert(new_frame_phys, vma.protection)
        .map_err(|_| kerr!(...))?,
};
```

- [ ] **Step 4: Build** — `make kernel-rv64`
- [ ] **Step 5: Commit** — `refactor(fault): migrate sync fault handler to pmap container API`

### Task 5: Migrate async fault handler to container API

**Files:**
- Modify: `kernel/src/mm/vm/fault_async.rs`

- [ ] **Step 1: Replace `pmap_extract_with_flags` fast path with `pmap.get()`**

Replace the fast-path check at lines 62-88 to use `pmap.get()`:
```rust
if let Some(ref entry) = pmap.get(fault_va_aligned) {
    if (!access_type.write || entry.permits_write())
        && (!access_type.execute || entry.permits_exec())
    {
        return Ok(());
    }
}
```

- [ ] **Step 2: Replace 4 `pmap_extract`+`pmap_enter` patterns with `entry_or_insert`**

Each instance of:
```rust
if pmap_extract(&pmap, va).is_none() { pmap_enter(&mut pmap, va, pa, perm, false)?; }
```
becomes:
```rust
pmap.entry_or_insert(va, pa, perm).map_err(|_| kerr!(...))?;
```

- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `refactor(fault): migrate async fault handler to pmap container API`

### Task 6: Full integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Smoke test** — `make agent-test`
- [ ] **Step 3: Full integration** — `make python-test-rv64`

This is the critical test — Phase A is a pure refactor with no behavioral change. If `python-test-rv64` passes, the container API is functionally equivalent to the old free-function API.
