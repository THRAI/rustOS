# VM-Pmap Unification — Phase C1: cow_fork_into + CloneFlags + do_clone

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move COW fork logic into `VmMap::cow_fork_into()`. Add `CloneFlags`. Create `do_clone()` that dispatches on flags.

**Architecture:** `cow_fork_into` iterates parent VMAs, dispatches on `entry.inheritance()` (Copy→COW/share, Share→share, None→skip), uses accountant methods (`rebind_store`, `set_cow_state`) and pmap container API (`range`, `for_each_in_range_mut`, `entry_or_insert`, `ShootdownBatch`). `do_clone` replaces `fork()` as the universal process/thread creation primitive.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-04-unified-clone.md`

**Depends on:** A1-A4 (pmap container), B1-B2 (VmMapping, builder, accountant)

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/mm/vm/map/mod.rs` | Modify | Add `cow_fork_into()`, `share_into()` |
| `kernel/src/proc/clone.rs` | Create | `CloneFlags`, `do_clone()`, replaces `fork.rs` |
| `kernel/src/proc/fork.rs` | Delete | Replaced by `clone.rs` |
| `kernel/src/proc/mod.rs` | Modify | Replace `mod fork` with `mod clone` |
| `kernel/src/syscall/process.rs` | Modify | Rewrite `sys_clone` to dispatch via `do_clone` |

---

### Task 1: Add `VmMap::cow_fork_into()`

**Files:**
- Modify: `kernel/src/mm/vm/map/mod.rs`

- [ ] **Step 1: Implement `cow_fork_into`**

Core logic:
```rust
impl VmMap {
    pub fn cow_fork_into(&mut self, child: &mut VmMap) -> Result<(), Errno> {
        // Phase 1: VMA-level work
        // Collect VMA info first to avoid borrow issues with self.iter()
        let vma_infos: Vec<_> = self.iter().map(|vma| {
            (vma.start(), vma.end(), vma.protection,
             vma.mapping.inheritance(), vma.mapping.clone())
        }).collect();

        for (start, end, prot, inherit, mapping) in &vma_infos {
            match inherit {
                VmInherit::Copy => {
                    let is_w = prot.contains(MapPerm::W);
                    if is_w {
                        // Build COW shadows
                        let (parent_shadow, child_shadow) = build_cow_shadows(mapping);
                        // Insert child with shadow + COW state
                        child.map(VirtAddr::new(*start as usize),
                                  VirtAddr::new(*end as usize), *prot, child_shadow)?;
                        child.set_cow_state(*start, CowState::CopyOnWrite)?;
                        // Rebind parent
                        self.rebind_store(*start, parent_shadow)?;
                        self.set_cow_state(*start, CowState::CopyOnWrite)?;
                    } else {
                        // RO: share object directly
                        child.map(VirtAddr::new(*start as usize),
                                  VirtAddr::new(*end as usize), *prot, mapping.clone())?;
                    }
                }
                VmInherit::Share => {
                    child.map(VirtAddr::new(*start as usize),
                              VirtAddr::new(*end as usize), *prot, mapping.clone())?;
                }
                VmInherit::None => {}
            }
        }

        // Phase 2: Pmap work (copy/downgrade pages)
        // ... (use pmap container API: range, for_each_in_range_mut, ShootdownBatch)
        Ok(())
    }
}
```

- [ ] **Step 2: Add `build_cow_shadows` helper**
- [ ] **Step 3: Add pmap phase (iterate parent pages, downgrade writable to RO, copy into child)**
- [ ] **Step 4: Build** — `make kernel-rv64`
- [ ] **Step 5: Commit** — `feat(vm): add VmMap::cow_fork_into()`

### Task 2: Add `VmMap::share_into()`

**Files:**
- Modify: `kernel/src/mm/vm/map/mod.rs`

- [ ] **Step 1: Implement share_into**

```rust
impl VmMap {
    pub fn share_into(&self) -> VmMap {
        VmMap::new(Arc::clone(&self.pmap))
    }
}
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `feat(vm): add VmMap::share_into() for CLONE_VM`

### Task 3: Create clone.rs with CloneFlags and do_clone

**Files:**
- Create: `kernel/src/proc/clone.rs`
- Modify: `kernel/src/proc/mod.rs`

- [ ] **Step 1: Create `clone.rs` with `CloneFlags` bitflags**

```rust
bitflags::bitflags! {
    pub struct CloneFlags: u64 {
        const VM             = 0x0000_0100;
        const FILES          = 0x0000_0400;
        const SIGHAND        = 0x0000_0800;
        const VFORK          = 0x0000_4000;
        const THREAD         = 0x0001_0000;
        const SETTLS         = 0x0008_0000;
        const PARENT_SETTID  = 0x0010_0000;
        const CHILD_CLEARTID = 0x0020_0000;
        const CHILD_SETTID   = 0x0100_0000;
    }
}
```

- [ ] **Step 2: Add `do_clone()` function** (initially just fork semantics, dispatching on flags)
- [ ] **Step 3: Update `proc/mod.rs`** — replace `mod fork` with `mod clone`
- [ ] **Step 4: Build** — `make kernel-rv64`
- [ ] **Step 5: Commit** — `feat(proc): add CloneFlags and do_clone, replace fork()`

### Task 4: Rewire sys_clone to use do_clone

**Files:**
- Modify: `kernel/src/syscall/process.rs`

- [ ] **Step 1: Rewrite `sys_clone` to parse flags and call `do_clone`**
- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `refactor(syscall): sys_clone dispatches via do_clone`

### Task 5: Delete fork.rs

**Files:**
- Delete: `kernel/src/proc/fork.rs`

- [ ] **Step 1: Verify no remaining imports of `fork.rs`**
- [ ] **Step 2: Delete the file**
- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `refactor(proc): delete fork.rs, replaced by clone.rs`

### Task 6: Full integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Smoke test** — `make agent-test`
- [ ] **Step 3: Full integration** — `make python-test-rv64`
