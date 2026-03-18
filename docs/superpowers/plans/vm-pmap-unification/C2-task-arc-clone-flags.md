# VM-Pmap Unification — Phase C2: Task Arc-Wrapping + CLONE_VM/FILES/SIGHAND/VFORK

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Arc-wrap shareable Task resources (`vm_map`, `fd_table`, `signal_actions`). Wire CLONE_VM, CLONE_FILES, CLONE_SIGHAND, and CLONE_VFORK support in `do_clone`.

**Architecture:** Task's `vm_map`, `fd_table`, and `signal_actions` become `Arc`-wrapped. `do_clone` dispatches: `CLONE_VM` → `Arc::clone(vm_map)`, `CLONE_FILES` → `Arc::clone(fd_table)`, `CLONE_SIGHAND` → `Arc::clone(signal_actions)`. `CLONE_VFORK` → parent blocks via `VforkDone` future until child exits or execs.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`

**Spec:** `docs/superpowers/specs/2026-03-16-vm-pmap-unification-04-unified-clone.md`

**Depends on:** C1 (do_clone, cow_fork_into)

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/proc/task.rs` | Modify | Arc-wrap `vm_map`, `fd_table`. Split `SignalState` to allow sharing actions. Add `vfork_done`. |
| `kernel/src/proc/clone.rs` | Modify | Wire CLONE_VM/FILES/SIGHAND/VFORK dispatch in `do_clone`. Add `VforkDone`. |
| Multiple callers | Modify | Update `task.vm_map` → `task.vm_map.read()`/`.write()` access patterns where Arc wrapping changes the type. |

---

### Task 1: Arc-wrap vm_map in Task

**Files:**
- Modify: `kernel/src/proc/task.rs`

- [ ] **Step 1: Change `vm_map` from `LeveledRwLock<VmMap, 1>` to `Arc<LeveledRwLock<VmMap, 1>>`**

```rust
// Before:
pub vm_map: LeveledRwLock<VmMap, 1>,
// After:
pub vm_map: Arc<LeveledRwLock<VmMap, 1>>,
```

- [ ] **Step 2: Update `Task::new()` and `Task::new_init()`**

```rust
vm_map: Arc::new(LeveledRwLock::new(VmMap::new(pmap))),
```

- [ ] **Step 3: Fix all callers that access `task.vm_map`**

Most callers already do `task.vm_map.read()` or `task.vm_map.write()`. These work unchanged because `Arc<LeveledRwLock<..>>` auto-derefs. The main change is where code takes `&task.vm_map` by reference — it now gets `&Arc<..>` which still derefs correctly.

- [ ] **Step 4: Build** — `make kernel-rv64`
- [ ] **Step 5: Commit** — `refactor(task): Arc-wrap vm_map for CLONE_VM support`

### Task 2: Arc-wrap fd_table in Task

**Files:**
- Modify: `kernel/src/proc/task.rs`

- [ ] **Step 1: Change `fd_table` from `Mutex<FdTable, 4>` to `Arc<Mutex<FdTable, 4>>`**
- [ ] **Step 2: Update `Task::new()` and `Task::new_init()`**
- [ ] **Step 3: Fix callers** — `task.fd_table.lock()` continues to work via deref.
- [ ] **Step 4: Build** — `make kernel-rv64`
- [ ] **Step 5: Commit** — `refactor(task): Arc-wrap fd_table for CLONE_FILES support`

### Task 3: Wire CLONE_VM and CLONE_FILES in do_clone

**Files:**
- Modify: `kernel/src/proc/clone.rs`

- [ ] **Step 1: Add CLONE_VM dispatch**

```rust
if flags.contains(CloneFlags::VM) {
    // Share address space — Arc::clone
    child.vm_map = Arc::clone(&parent.vm_map);
} else {
    // COW fork
    let mut parent_vm = parent.vm_map.write();
    let mut child_vm = child.vm_map.write();
    parent_vm.cow_fork_into(&mut child_vm)?;
}
```

- [ ] **Step 2: Add CLONE_FILES dispatch**

```rust
if flags.contains(CloneFlags::FILES) {
    child.fd_table = Arc::clone(&parent.fd_table);
} else {
    *child.fd_table.lock() = parent.fd_table.lock().fork();
}
```

- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(clone): wire CLONE_VM and CLONE_FILES`

### Task 4: Add VforkDone and wire CLONE_VFORK

**Files:**
- Modify: `kernel/src/proc/clone.rs`
- Modify: `kernel/src/proc/task.rs`

- [ ] **Step 1: Add `VforkDone` struct**

```rust
pub struct VforkDone {
    done: AtomicBool,
    waker: Mutex<Option<core::task::Waker>, 4>,
}

impl VforkDone {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            done: AtomicBool::new(false),
            waker: Mutex::new(None),
        })
    }
    pub fn signal(&self) {
        self.done.store(true, Ordering::Release);
        if let Some(w) = self.waker.lock().take() { w.wake(); }
    }
}
```

- [ ] **Step 2: Add `vfork_done: Option<Arc<VforkDone>>` to Task**
- [ ] **Step 3: In do_clone, if CLONE_VFORK: create VforkDone, set on child**
- [ ] **Step 4: In sys_clone, if CLONE_VFORK: await vfork_done after spawn**
- [ ] **Step 5: Signal vfork_done from child's exec and exit paths**
- [ ] **Step 6: Build** — `make kernel-rv64`
- [ ] **Step 7: Commit** — `feat(clone): add VforkDone and CLONE_VFORK support`

### Task 5: Wire CLONE_SIGHAND (optional, can defer)

**Files:**
- Modify: `kernel/src/proc/task.rs`
- Modify: `kernel/src/proc/clone.rs`

- [ ] **Step 1: Extract signal actions into Arc-wrappable type**
- [ ] **Step 2: Add CLONE_SIGHAND dispatch in do_clone**
- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(clone): wire CLONE_SIGHAND`

### Task 6: Full integration verification

- [ ] **Step 1: Full build** — `make kernel-rv64`
- [ ] **Step 2: Smoke test** — `make agent-test`
- [ ] **Step 3: Full integration** — `make python-test-rv64`
- [ ] **Step 4: Test vfork workloads** — run busybox commands that use vfork (sh, system())
