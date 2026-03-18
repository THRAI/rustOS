# VM-Pmap Unification — Phase A3: ShootdownBatch + for_each_in_range_mut

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ShootdownBatch` (quiesce-on-create, collect tokens, flush-on-drop), `OccupiedEntryMut`, and `Pmap::for_each_in_range_mut()` with recursive subtree-skipping walk.

**Architecture:** `ShootdownBatch` wraps `QuiesceGuard` + a `Vec<VirtAddr>` of dirty pages. `OccupiedEntryMut` borrows the PTE slot (not `&mut Pmap`) so the walker can yield entries without borrow conflict. `for_each_in_range_mut` uses a recursive walk parameterized by Sv39 level.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-01-pmap-container.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/mm/pmap/shootdown.rs` | Modify | Add `ShootdownToken`, `ShootdownBatch` |
| `kernel/src/mm/pmap/container.rs` | Modify | Add `OccupiedEntryMut` |
| `kernel/src/mm/pmap/mod.rs` | Modify | Add `Pmap::for_each_in_range_mut()` |

---

### Task 1: Add ShootdownBatch to shootdown.rs

**Files:**
- Modify: `kernel/src/mm/pmap/shootdown.rs`

- [ ] **Step 1: Add `ShootdownBatch` struct with `new()` and `add()`**

```rust
/// Batched TLB invalidation with quiesce protocol.
///
/// 1. new() → QuiesceGuard parks remote harts
/// 2. add() → collect tokens from entry mutations
/// 3. drop() → adaptive local flush + release remote harts
#[cfg(target_arch = "riscv64")]
pub struct ShootdownBatch {
    dirty_vas: alloc::vec::Vec<usize>,
    asid: u16,
    _quiesce: QuiesceGuard,
}

#[cfg(target_arch = "riscv64")]
impl ShootdownBatch {
    pub fn new(active: &[AtomicBool; MAX_CPUS], asid: u16) -> Self {
        Self {
            dirty_vas: alloc::vec::Vec::new(),
            asid,
            _quiesce: QuiesceGuard::new(active),
        }
    }

    pub fn add(&mut self, token: ShootdownToken) {
        self.dirty_vas.push(token.0.as_usize());
    }
}

#[cfg(target_arch = "riscv64")]
impl Drop for ShootdownBatch {
    fn drop(&mut self) {
        if !self.dirty_vas.is_empty() {
            if self.dirty_vas.len() <= SHOOTDOWN_PAGE_THRESHOLD {
                for &va in &self.dirty_vas {
                    unsafe {
                        core::arch::asm!("sfence.vma {}, {}",
                            in(reg) va, in(reg) self.asid as usize);
                    }
                }
            } else {
                crate::hal::flush_asid(self.asid as usize);
            }
        }
        // _quiesce drops → releases remote harts → they flush_all
    }
}
```

- [ ] **Step 2: Add non-riscv64 stub** (for compile-time on host)

```rust
#[cfg(not(target_arch = "riscv64"))]
pub struct ShootdownBatch;

#[cfg(not(target_arch = "riscv64"))]
impl ShootdownBatch {
    pub fn new(_active: &[AtomicBool; 8], _asid: u16) -> Self { Self }
    pub fn add(&mut self, _token: ShootdownToken) {}
}
```

- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(pmap): add ShootdownBatch for batched TLB invalidation`

### Task 2: Add OccupiedEntryMut to container.rs

**Files:**
- Modify: `kernel/src/mm/pmap/container.rs`

- [ ] **Step 1: Add OccupiedEntryMut struct + methods**

```rust
/// Mutable entry yielded during range traversal.
/// Borrows PTE slot directly, NOT &mut Pmap.
pub struct OccupiedEntryMut<'a> {
    pub(super) pte_ptr: *mut u64,
    pub(super) va: VirtAddr,
    pub(super) pa: PhysAddr,
    pub(super) flags: PteFlags,
    pub(super) _lifetime: PhantomData<&'a mut u64>,
}

impl OccupiedEntryMut<'_> {
    pub fn va(&self) -> VirtAddr { self.va }
    pub fn pa(&self) -> PhysAddr { self.pa }
    pub fn flags(&self) -> PteFlags { self.flags }

    /// Remove PTE. Returns token for batched flush.
    pub fn remove(self) -> super::shootdown::ShootdownToken {
        unsafe { self.pte_ptr.write_volatile(0); }
        super::shootdown::ShootdownToken(self.va)
    }

    /// Change permissions. Returns token for batched flush.
    pub fn set_perm(&mut self, perm: MapPerm) -> super::shootdown::ShootdownToken {
        let sw_bits = self.flags & (PteFlags::SW_WIRED | PteFlags::SW_MANAGED);
        let new_flags = map_perm_to_pte_flags(perm) | sw_bits;
        unsafe {
            self.pte_ptr.write_volatile(encode_pte(self.pa.as_usize(), new_flags));
        }
        self.flags = new_flags;
        super::shootdown::ShootdownToken(self.va)
    }
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `feat(pmap): add OccupiedEntryMut for mutable range traversal`

### Task 3: Implement `for_each_in_range_mut`

**Files:**
- Modify: `kernel/src/mm/pmap/mod.rs`

- [ ] **Step 1: Add `for_each_in_range_mut` method + internal recursive walker**

```rust
impl Pmap {
    /// Visit valid leaf PTEs in [start, end), skipping empty subtrees.
    pub fn for_each_in_range_mut<F>(
        &mut self, start: VirtAddr, end: VirtAddr, mut f: F,
    ) where F: FnMut(container::OccupiedEntryMut<'_>) {
        let root_pa = self.root.phys().as_usize();
        unsafe {
            Self::walk_range_mut(root_pa, start.as_usize(), end.as_usize(), 0, &mut f);
        }
    }
}

impl Pmap {
    unsafe fn walk_range_mut<F>(
        table_pa: usize, start: usize, end: usize, level: usize, f: &mut F,
    ) where F: FnMut(container::OccupiedEntryMut<'_>) {
        let shift = 12 + 9 * (SV39_LEVELS - 1 - level);
        let span = 1usize << shift;
        let start_idx = (start >> shift) & 0x1FF;
        let end_idx = ((end - 1) >> shift) & 0x1FF;

        for idx in start_idx..=end_idx {
            let pte_ptr = (table_pa as *mut u64).add(idx);
            let raw = pte_ptr.read_volatile();

            if !pte_is_valid(raw) { continue; }

            if pte_is_leaf(raw) {
                let va_base = (start & !(span - 1)) | (idx << shift);
                f(container::OccupiedEntryMut {
                    pte_ptr,
                    va: VirtAddr::new(va_base),
                    pa: PhysAddr::new(pte_pa(raw)),
                    flags: pte_flags(raw),
                    _lifetime: core::marker::PhantomData,
                });
                continue;
            }

            // Non-leaf: descend with clipped range
            if level < SV39_LEVELS - 1 {
                let child_start = core::cmp::max(start, idx << shift);
                let child_end = core::cmp::min(end, (idx + 1) << shift);
                Self::walk_range_mut(pte_pa(raw), child_start, child_end, level + 1, f);
            }
        }
    }
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `feat(pmap): add for_each_in_range_mut with subtree skipping`

### Task 4: Integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Smoke test** — `make agent-test`
