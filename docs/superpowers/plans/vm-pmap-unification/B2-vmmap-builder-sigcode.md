# VM-Pmap Unification — Phase B2: VmMap Builder + Sigcode VMA + Accountant Methods

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `VmMap::map()` builder function. Migrate sys_mmap and exec to use it. Add `map_sigcode()` for sigcode VMA. Add `rebind_store()` and `set_cow_state()` accountant methods.

**Architecture:** `map()` takes `VmMapping` enum + `(start, end, perm)`, validates, constructs `VmMapEntry` internally, and inserts. `map_sigcode()` uses `VmMapping::Shared` with a global `SIGCODE_OBJ`. `rebind_store` and `set_cow_state` are accountant methods that modify entry fields and bump timestamp.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-03-vmmap-builder-accountant.md`

**Depends on:** B1 (VmMapping enum, VmMapEntry restructure)

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/mm/vm/map/mod.rs` | Modify | Add `map()`, `map_or_merge()`, `map_sigcode()`, `rebind_store()`, `set_cow_state()` |
| `kernel/src/syscall/memory.rs` | Modify | Migrate `sys_mmap` to use `vm.map()` |
| `kernel/src/proc/exec.rs` | Modify | Migrate PT_LOAD/stack/interp to `vm.map()` or `vm.map_or_merge()` |
| `kernel/src/proc/signal.rs` | Modify | Add `SIGCODE_OBJ`, `init_sigcode_object()`. Remove `map_sigcode_page()`. |

---

### Task 1: Add `VmMap::map()` builder

**Files:**
- Modify: `kernel/src/mm/vm/map/mod.rs`

- [ ] **Step 1: Add `map()` method**

```rust
impl VmMap {
    pub fn map(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        perm: MapPerm,
        mapping: VmMapping,
    ) -> Result<(), Errno> {
        let entry = VmMapEntry::new(
            start.as_usize() as u64,
            end.as_usize() as u64,
            mapping,
            perm,
        );
        self.insert_entry(entry)
    }
}
```

- [ ] **Step 2: Add `map_or_merge()` for exec (absorbs insert_or_merge_file_vma)**

```rust
impl VmMap {
    pub fn map_or_merge(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        perm: MapPerm,
        mapping: VmMapping,
    ) -> Result<(), Errno> {
        let s = start.as_usize() as u64;
        let e = end.as_usize() as u64;
        // Check for exact overlap with matching offset
        if let Some(existing) = self.lookup_mut(s) {
            if existing.start() == s && existing.end() == e {
                let same_offset = existing.mapping.offset() == mapping.offset();
                if same_offset {
                    existing.protection |= perm;
                    return Ok(());
                }
            }
        }
        self.map(start, end, perm, mapping)
    }
}
```

- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(vm): add VmMap::map() and map_or_merge() builder methods`

### Task 2: Migrate sys_mmap to use VmMap::map()

**Files:**
- Modify: `kernel/src/syscall/memory.rs:289-307`

- [ ] **Step 1: Replace VmMapEntry::new + insert_entry with map()**

```rust
// Replace:
//   let vma = VmMapEntry::new(base, end, BackingStore::Object{object,offset},
//                             EntryFlags::empty(), perm);
//   vm.insert_entry(vma)?;
// With:
let mapping = if map_anon {
    VmMapping::AnonPrivate { object }
} else if map_private {
    VmMapping::FilePrivate { object, offset }
} else {
    VmMapping::FileShared { object, offset }
};
vm.map(VirtAddr::new(base), VirtAddr::new(base + aligned_len), perm, mapping)?;
```

- [ ] **Step 2: Build** — `make kernel-rv64`
- [ ] **Step 3: Commit** — `refactor(syscall): migrate sys_mmap to VmMap::map()`

### Task 3: Migrate exec to use VmMap::map()

**Files:**
- Modify: `kernel/src/proc/exec.rs`

- [ ] **Step 1: Migrate PT_LOAD VMA creation (exec.rs:199-213)**

Replace `VmMapEntry::new(...)` + `insert_or_merge_file_vma(...)` with:
```rust
let mapping = VmMapping::FilePrivate {
    object: obj,
    offset: file_offset_page_aligned as u64,
};
new_vm.map_or_merge(
    VirtAddr::new(va_start), VirtAddr::new(va_end), region.prot, mapping,
)?;
```

- [ ] **Step 2: Migrate stack VMA (exec.rs:256-266)**

```rust
new_vm.map(
    VirtAddr::new(stack_bottom), VirtAddr::new(USER_STACK_TOP),
    crate::map_perm!(R, W, U),
    VmMapping::AnonPrivate { object: stack_obj },
)?;
```

- [ ] **Step 3: Migrate interp PT_LOAD VMAs (exec.rs:561-572)**

Same pattern as main PT_LOAD: `VmMapping::FilePrivate` + `map_or_merge`.

- [ ] **Step 4: Delete `insert_or_merge_file_vma` function**
- [ ] **Step 5: Build** — `make kernel-rv64`
- [ ] **Step 6: Commit** — `refactor(exec): migrate to VmMap::map()`

### Task 4: Add accountant methods

**Files:**
- Modify: `kernel/src/mm/vm/map/mod.rs`

- [ ] **Step 1: Add `rebind_store()`**

```rust
impl VmMap {
    pub fn rebind_store(
        &mut self, va: u64,
        new_object: Arc<LeveledRwLock<VmObject, 3>>,
    ) -> Result<(), Errno> {
        let vma = self.lookup_mut(va).ok_or(Errno::Esrch)?;
        match &mut vma.mapping {
            VmMapping::AnonPrivate { object }
            | VmMapping::FilePrivate { object, .. }
            | VmMapping::Heap { object } => *object = new_object,
            _ => return Err(Errno::Einval),
        }
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
```

- [ ] **Step 2: Add `set_cow_state()`**

```rust
impl VmMap {
    pub fn set_cow_state(&mut self, va: u64, state: CowState) -> Result<(), Errno> {
        let vma = self.lookup_mut(va).ok_or(Errno::Esrch)?;
        if state == CowState::CopyOnWrite
            && vma.mapping.inheritance() != VmInherit::Copy
        {
            return Err(Errno::Einval);
        }
        vma.cow_state = state;
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
```

- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(vm): add rebind_store and set_cow_state accountant methods`

### Task 5: Add sigcode VMA

**Files:**
- Modify: `kernel/src/proc/signal.rs`
- Modify: `kernel/src/mm/vm/map/mod.rs`
- Modify: `kernel/src/proc/exec.rs` (replace `map_sigcode_page`)

- [ ] **Step 1: Add `SIGCODE_OBJ` and `init_sigcode_object()` to signal.rs**
- [ ] **Step 2: Add `VmMap::map_sigcode()` to `mm/vm/map/mod.rs`**
- [ ] **Step 3: Replace `map_sigcode_page(&mut pmap)` in exec.rs:402 with `new_vm.map_sigcode()`**
- [ ] **Step 4: Remove sigcode special-case in fork.rs:43-60** (now handled by VmInherit::Share)
- [ ] **Step 5: Build** — `make kernel-rv64`
- [ ] **Step 6: Smoke test** — `make agent-test`
- [ ] **Step 7: Commit** — `feat(vm): sigcode gets proper VMA via VmMapping::Shared`

### Task 6: Full integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Smoke test** — `make agent-test`
- [ ] **Step 3: Full integration** — `make python-test-rv64`
