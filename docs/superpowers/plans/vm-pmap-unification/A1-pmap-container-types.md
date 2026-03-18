# VM-Pmap Unification — Phase A1: Pmap Container Types + get/entry

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the core pmap container types (`PmapRef`, `OccupiedEntry`, `VacantEntry`, `PmapEntry`) and implement `Pmap::get()` and `Pmap::entry()`.

**Architecture:** New file `mm/pmap/container.rs` holds all container types. `get()` wraps the existing `walk` function with `alloc=false` and returns `PmapRef`. `entry()` wraps `walk` similarly and returns `Occupied` or `Vacant` depending on the PTE state. `VacantEntry::insert()` re-walks with `alloc=true`. `OccupiedEntry` borrows the PTE slot via `*mut u64`, not `&mut Pmap`.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-01-pmap-container.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/mm/pmap/container.rs` | Create | `PmapRef`, `PmapEntry`, `OccupiedEntry`, `VacantEntry` types + methods |
| `kernel/src/mm/pmap/mod.rs` | Modify | Add `pub mod container;`, add `get()` and `entry()` and `entry_or_insert()` methods on `Pmap` |

---

### Task 1: Create container.rs with PmapRef

**Files:**
- Create: `kernel/src/mm/pmap/container.rs`
- Modify: `kernel/src/mm/pmap/mod.rs` — add `pub mod container;`

- [ ] **Step 1: Create `container.rs` with `PmapRef` struct and methods**

```rust
//! Pmap container types: typed views into the hardware page table.

use crate::hal_common::{PhysAddr, VirtAddr};
use super::pte::PteFlags;

/// Read-only view of a valid leaf PTE.
#[derive(Debug, Clone, Copy)]
pub struct PmapRef {
    pub(super) pa: PhysAddr,
    pub(super) flags: PteFlags,
}

impl PmapRef {
    pub fn pa(&self) -> PhysAddr { self.pa }
    pub fn flags(&self) -> PteFlags { self.flags }
    pub fn permits_write(&self) -> bool { self.flags.contains(PteFlags::W) }
    pub fn permits_exec(&self) -> bool { self.flags.contains(PteFlags::X) }
}
```

- [ ] **Step 2: Add `pub mod container;` to `mm/pmap/mod.rs`**
- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(pmap): add PmapRef container type`

### Task 2: Add `Pmap::get()`

**Files:**
- Modify: `kernel/src/mm/pmap/mod.rs` — add `get()` method on `Pmap`

- [ ] **Step 1: Implement `get` on Pmap**

```rust
impl Pmap {
    /// Read-only lookup. Returns None if no valid leaf PTE.
    pub fn get(&self, va: VirtAddr) -> Option<container::PmapRef> {
        unsafe {
            let pte_ptr = walk::walk::<SV39_LEVELS>(
                self.root.phys(), va, false, &mut |_| None,
            )?;
            let raw = pte_ptr.read_volatile();
            if pte_is_valid(raw) && pte_is_leaf(raw) {
                Some(container::PmapRef {
                    pa: PhysAddr::new(pte_pa(raw) | va.page_offset()),
                    flags: pte_flags(raw),
                })
            } else {
                None
            }
        }
    }
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `feat(pmap): add Pmap::get() read-only lookup`

### Task 3: Add OccupiedEntry and VacantEntry types

**Files:**
- Modify: `kernel/src/mm/pmap/container.rs`

- [ ] **Step 1: Add OccupiedEntry, VacantEntry, PmapEntry to container.rs**

```rust
use core::marker::PhantomData;
use super::{Pmap, pte::{encode_pte, map_perm_to_pte_flags, PteFlags}};
use crate::mm::vm::MapPerm;

/// Mutable handle to an existing leaf PTE.
/// Borrows the PTE slot, NOT &mut Pmap.
pub struct OccupiedEntry<'a> {
    pub(super) pte_ptr: *mut u64,
    pub(super) pmap: &'a mut Pmap,
    pub(super) va: VirtAddr,
    pub(super) pa: PhysAddr,
    pub(super) flags: PteFlags,
}

/// Handle to an absent PTE slot.
pub struct VacantEntry<'a> {
    pub(super) pmap: &'a mut Pmap,
    pub(super) va: VirtAddr,
}

pub enum PmapEntry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}
```

- [ ] **Step 2: Implement OccupiedEntry methods**

```rust
impl OccupiedEntry<'_> {
    pub fn va(&self) -> VirtAddr { self.va }
    pub fn pa(&self) -> PhysAddr { self.pa }
    pub fn flags(&self) -> PteFlags { self.flags }

    /// Overwrite mapping with new PA + perms.
    pub fn insert(&mut self, pa: PhysAddr, perm: MapPerm) {
        let flags = map_perm_to_pte_flags(perm)
            | (self.flags & (PteFlags::SW_WIRED | PteFlags::SW_MANAGED));
        unsafe { self.pte_ptr.write_volatile(encode_pte(pa.as_usize(), flags)); }
        self.pa = pa;
        self.flags = flags;
        // Shootdown for single VA
        #[cfg(target_arch = "riscv64")]
        super::shootdown::pmap_shootdown(
            &self.pmap.active,
            self.va.as_usize(),
            self.va.as_usize() + crate::hal_common::PAGE_SIZE,
            self.pmap.asid,
        );
    }

    /// Upgrade perms in place (COW promote).
    pub fn promote(&mut self, new_perm: MapPerm) {
        let sw_bits = self.flags & (PteFlags::SW_WIRED | PteFlags::SW_MANAGED);
        let new_flags = map_perm_to_pte_flags(new_perm) | sw_bits;
        unsafe { self.pte_ptr.write_volatile(encode_pte(self.pa.as_usize(), new_flags)); }
        self.flags = new_flags;
        #[cfg(target_arch = "riscv64")]
        super::shootdown::pmap_shootdown(
            &self.pmap.active,
            self.va.as_usize(),
            self.va.as_usize() + crate::hal_common::PAGE_SIZE,
            self.pmap.asid,
        );
    }

    /// Remove mapping. Returns token for batched TLB flush.
    pub fn remove(self) -> super::shootdown::ShootdownToken {
        unsafe { self.pte_ptr.write_volatile(0); }
        self.pmap.stats.resident_count = self.pmap.stats.resident_count.saturating_sub(1);
        if self.flags.contains(PteFlags::SW_WIRED) {
            self.pmap.stats.wired_count = self.pmap.stats.wired_count.saturating_sub(1);
        }
        #[cfg(target_arch = "riscv64")]
        super::shootdown::pmap_shootdown(
            &self.pmap.active,
            self.va.as_usize(),
            self.va.as_usize() + crate::hal_common::PAGE_SIZE,
            self.pmap.asid,
        );
        super::shootdown::ShootdownToken(self.va)
    }
}
```

- [ ] **Step 3: Implement VacantEntry::insert**

```rust
impl VacantEntry<'_> {
    /// Install new mapping. Allocates intermediate PT pages if needed.
    pub fn insert(self, pa: PhysAddr, perm: MapPerm) -> Result<(), ()> {
        // Delegate to existing pmap_enter logic
        super::pmap_enter(self.pmap, self.va, pa, perm, false)
    }
}
```

- [ ] **Step 4: Build** — `make kernel-rv64`
- [ ] **Step 5: Commit** — `feat(pmap): add OccupiedEntry/VacantEntry/PmapEntry types`

### Task 4: Add `Pmap::entry()` and `entry_or_insert()`

**Files:**
- Modify: `kernel/src/mm/pmap/mod.rs`

- [ ] **Step 1: Implement `entry()` on Pmap**

```rust
impl Pmap {
    pub fn entry(&mut self, va: VirtAddr) -> container::PmapEntry<'_> {
        unsafe {
            let pte_ptr = walk::walk::<SV39_LEVELS>(
                self.root.phys(), va, false, &mut |_| None,
            );
            match pte_ptr {
                Some(ptr) => {
                    let raw = ptr.read_volatile();
                    if pte_is_valid(raw) && pte_is_leaf(raw) {
                        container::PmapEntry::Occupied(container::OccupiedEntry {
                            pte_ptr: ptr,
                            pmap: self,
                            va,
                            pa: PhysAddr::new(pte_pa(raw)),
                            flags: pte_flags(raw),
                        })
                    } else {
                        container::PmapEntry::Vacant(container::VacantEntry {
                            pmap: self,
                            va,
                        })
                    }
                }
                None => container::PmapEntry::Vacant(container::VacantEntry {
                    pmap: self,
                    va,
                }),
            }
        }
    }

    pub fn entry_or_insert(
        &mut self, va: VirtAddr, pa: PhysAddr, perm: MapPerm,
    ) -> Result<(), ()> {
        match self.entry(va) {
            container::PmapEntry::Occupied(_) => Ok(()),
            container::PmapEntry::Vacant(v) => v.insert(pa, perm),
        }
    }
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Smoke test** — `make agent-test`
- [ ] **Step 4: Commit** — `feat(pmap): add Pmap::entry() and entry_or_insert()`

### Task 5: Add ShootdownToken to shootdown.rs

**Files:**
- Modify: `kernel/src/mm/pmap/shootdown.rs`

- [ ] **Step 1: Add ShootdownToken struct**

```rust
/// Proof that a PTE was modified. Carries VA for flush.
pub struct ShootdownToken(pub(super) crate::hal_common::VirtAddr);

impl ShootdownToken {
    pub fn va(&self) -> crate::hal_common::VirtAddr { self.0 }
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `feat(pmap): add ShootdownToken type`

### Task 6: Integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Boot test** — `make agent-test`
- [ ] **Step 3: Integration test** — `make python-test-rv64` (no behavioral change)
