# VM-Pmap Unification — Phase A2: PmapRange Iterator + Subtree Skipping

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only range iterator `Pmap::range()` that walks Sv39 page tables and skips empty subtrees (invalid L0 skips 1GB, invalid L1 skips 2MB).

**Architecture:** `PmapRange` holds `&Pmap`, a stack of `(table_pa, current_idx, end_idx)` per level, and the current depth. `Iterator::next()` advances through the tree, descending into valid non-leaf PTEs and skipping invalid ones. Leaf PTEs yield `(VirtAddr, PhysAddr, PteFlags)`.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-01-pmap-container.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/mm/pmap/container.rs` | Modify | Add `PmapRange` struct + `Iterator` impl |
| `kernel/src/mm/pmap/mod.rs` | Modify | Add `Pmap::range()` method |

---

### Task 1: Add PmapRange struct

**Files:**
- Modify: `kernel/src/mm/pmap/container.rs`

- [ ] **Step 1: Add PmapRange type with traversal state**

```rust
use super::walk::vpn_index;
use super::pte::{pte_is_valid, pte_is_leaf, pte_pa, pte_flags};
use super::SV39_LEVELS;

/// Read-only range iterator over valid leaf PTEs in [start, end).
/// Skips empty subtrees: invalid L0 entry skips 1GB, invalid L1 skips 2MB.
pub struct PmapRange<'a> {
    _pmap: &'a super::Pmap,
    root_pa: usize,
    end_va: usize,
    /// Per-level state: (table_pa, current_idx, end_idx_inclusive)
    /// Level 0 = root (1GB), Level 1 = dir (2MB), Level 2 = leaf (4KB)
    stack: [(usize, usize, usize); SV39_LEVELS],
    depth: usize,
    started: bool,
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `feat(pmap): add PmapRange struct`

### Task 2: Implement Iterator for PmapRange

**Files:**
- Modify: `kernel/src/mm/pmap/container.rs`

- [ ] **Step 1: Implement Iterator::next with subtree skipping**

The iterator state machine:
1. Start at current depth in the stack
2. Read PTE at `stack[depth].current_idx`
3. If invalid: advance index, if past end: pop up a level
4. If valid leaf: yield (va, pa, flags), advance index
5. If valid non-leaf (depth < 2): push down to child, compute child index range

```rust
impl<'a> Iterator for PmapRange<'a> {
    type Item = (VirtAddr, PhysAddr, PteFlags);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.depth >= SV39_LEVELS {
                return None;
            }

            let (table_pa, ref mut idx, end_idx) = self.stack[self.depth];
            if *idx > end_idx {
                // Exhausted this level, pop up
                if self.depth == 0 {
                    return None;
                }
                self.depth -= 1;
                self.stack[self.depth].1 += 1; // advance parent
                continue;
            }

            let pte_ptr = (table_pa as *const u64).wrapping_add(*idx);
            let raw = unsafe { pte_ptr.read_volatile() };

            if !pte_is_valid(raw) {
                *idx += 1;
                continue; // subtree skip
            }

            if pte_is_leaf(raw) {
                let va = self.compute_va(self.depth, *idx);
                let pa = PhysAddr::new(pte_pa(raw));
                let flags = pte_flags(raw);
                *idx += 1;
                if va >= self.end_va {
                    return None;
                }
                return Some((VirtAddr::new(va), pa, flags));
            }

            // Non-leaf: descend
            if self.depth < SV39_LEVELS - 1 {
                let child_table = pte_pa(raw);
                let child_depth = self.depth + 1;
                let (child_start_idx, child_end_idx) =
                    self.compute_child_range(child_depth, *idx);
                self.stack[child_depth] = (child_table, child_start_idx, child_end_idx);
                self.depth = child_depth;
            } else {
                *idx += 1;
            }
        }
    }
}
```

- [ ] **Step 2: Add helper methods `compute_va` and `compute_child_range`**

```rust
impl PmapRange<'_> {
    /// Compute the VA for a given level and index.
    fn compute_va(&self, level: usize, idx: usize) -> usize {
        // Reconstruct VA from all stack indices up to and including this level
        let mut va = 0usize;
        for l in 0..=level {
            let shift = 12 + 9 * (SV39_LEVELS - 1 - l);
            va |= self.stack[l].1 << shift;
        }
        // For the current leaf, use idx instead of stack value
        let shift = 12 + 9 * (SV39_LEVELS - 1 - level);
        va &= !(0x1FF << shift);
        va |= idx << shift;
        va
    }

    fn compute_child_range(&self, child_depth: usize, _parent_idx: usize) -> (usize, usize) {
        let shift = 12 + 9 * (SV39_LEVELS - 1 - child_depth);
        let start_idx = (/* start_va */ 0 >> shift) & 0x1FF;
        // Clip to range [start_va, end_va)
        // First child of first parent: clip start
        // Last child of last parent: clip end
        // Middle parents: full 0..511
        // (detailed clipping logic based on start_va/end_va)
        let end_idx = 511; // simplified; real impl clips to end_va
        (start_idx, end_idx)
    }
}
```

Note: The actual `compute_va` and `compute_child_range` implementations require tracking `start_va` in the struct and clipping index ranges at boundary levels. The implementer should store `start_va` in `PmapRange` and use it for clipping.

- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(pmap): implement Iterator for PmapRange with subtree skipping`

### Task 3: Add `Pmap::range()` constructor

**Files:**
- Modify: `kernel/src/mm/pmap/mod.rs`

- [ ] **Step 1: Add `range()` method to Pmap**

```rust
impl Pmap {
    /// Read-only range iteration with subtree skipping.
    pub fn range(&self, start: VirtAddr, end: VirtAddr) -> container::PmapRange<'_> {
        let root_pa = self.root.phys().as_usize();
        let start_l0_idx = walk::vpn_index::<SV39_LEVELS>(start, 0);
        let end_l0_idx = if end.as_usize() == 0 { 511 }
            else { walk::vpn_index::<SV39_LEVELS>(VirtAddr::new(end.as_usize() - 1), 0) };

        container::PmapRange {
            _pmap: self,
            root_pa,
            end_va: end.as_usize(),
            stack: [
                (root_pa, start_l0_idx, end_l0_idx),
                (0, 0, 0),
                (0, 0, 0),
            ],
            depth: 0,
            started: false,
        }
    }
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `feat(pmap): add Pmap::range() constructor`

### Task 4: Integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Smoke test** — `make agent-test`
