# VM-Pmap Unification — Unified Clone and Task Changes

**Date:** 2026-03-16
**Status:** Implemented
**Parent:** `00-overview`
**Phase:** C
**Depends on:** `01-pmap-container`, `02-vmmapping-entry`, `03-vmmap-builder-accountant`

## Motivation

Process creation is a single `fork()` function (`fork.rs:19-112`) with no
flag-based differentiation. `sys_clone` (`process.rs:52-91`) ignores
`CLONE_VM`, `CLONE_VFORK`, `CLONE_FILES`, `CLONE_SIGHAND`, and rejects
`CLONE_THREAD`. Fork COW setup (`cow_fork_vm`, `fork.rs:124-258`) is a
free function that bypasses VmMap's accountant methods.

Replace `fork()` with a flag-driven `do_clone()` primitive. Move COW fork
logic into `VmMap::cow_fork_into()`. Arc-wrap shareable Task resources.

## CloneFlags

```rust
bitflags! {
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

## `do_clone`

Replaces `fork()` at `fork.rs:19-112`. Dispatches on flags.

```rust
pub fn do_clone(
    parent: &Arc<Task>,
    flags: CloneFlags,
    child_stack: usize,
    parent_tidptr: usize,
    child_tidptr: usize,
    tls: usize,
) -> KernelResult<Arc<Task>> {
    // 1. Validate flag combinations
    if flags.contains(THREAD) && !flags.contains(VM | SIGHAND) {
        return Err(Errno::Einval);
    }

    // 2. Create child Task
    let child = Task::new(Arc::downgrade(parent));

    // 3. Address space
    if flags.contains(VM) {
        child.set_vm_map(parent.vm_map_arc());  // Arc::clone
    } else {
        let mut parent_vm = parent.vm_map.write();
        let mut child_vm = child.vm_map.write();
        parent_vm.cow_fork_into(&mut child_vm)?;
    }

    // 4. File descriptors
    if flags.contains(FILES) {
        child.share_fd_table(parent);  // Arc::clone
    } else {
        *child.fd_table.lock() = parent.fd_table.lock().fork();
    }

    // 5. Signal handlers
    if flags.contains(SIGHAND) {
        child.share_signal_actions(parent);  // Arc::clone
    } else {
        *child.signal_actions.lock() = *parent.signal_actions.lock();
    }

    // 6. Trap frame + stack + TLS
    {
        let mut tf = child.trap_frame.lock();
        *tf = *parent.trap_frame.lock();
        tf.set_ret_val(0);
        tf.advance_pc();
        if child_stack != 0 { tf.x[2] = child_stack; }
        if flags.contains(SETTLS) && tls != 0 { tf.x[4] = tls; }
    }

    // 7. Copy remaining per-process state
    child.brk.store(parent.brk.load(Relaxed), Relaxed);
    *child.cwd.lock() = parent.cwd.lock().clone();
    child.signals_pending.store(SigSet::empty(), Relaxed);
    child.signals_blocked.store(
        parent.signals_blocked.load(Relaxed), Relaxed);

    // 8. Parent-child linkage
    parent.children.lock().push(Arc::clone(&child));

    Ok(child)
}
```

## `sys_clone` Becomes Thin Dispatch

```rust
pub fn sys_clone(task, flags, child_stack, parent_tid, tls, child_tid)
    -> KernelResult<usize>
{
    let flags = CloneFlags::from_bits_truncate(flags as u64);
    let child = do_clone(task, flags, child_stack, parent_tid, child_tid, tls)?;
    let pid = child.pid;
    spawn_user_task(child, current().cpu_id);

    if flags.contains(CloneFlags::VFORK) {
        // Block parent until child exits or execs
        // vfork_done.await
    }

    Ok(pid as usize)
}
```

## `VmMap::cow_fork_into`

Replaces the free function `cow_fork_vm` at `fork.rs:124-258`. Composes
accountant methods (spec 03) and pmap container API (spec 01).

```rust
impl VmMap {
    pub fn cow_fork_into(&mut self, child: &mut VmMap) -> Result<(), Errno> {
        // Phase 1: VMA-level work
        for vma in self.iter() {
            match vma.inheritance() {
                VmInherit::Copy => {
                    let is_w = vma.protection.contains(MapPerm::W);
                    let (parent_shadow, child_store) = if is_w {
                        let (ps, cs) = build_cow_shadows(&vma.mapping);
                        (Some(ps), cs)
                    } else {
                        (None, vma.mapping.clone())
                    };

                    // Insert child entry
                    child.map(
                        VirtAddr(vma.start()), VirtAddr(vma.end()),
                        vma.protection, child_store,
                    )?;
                    if is_w {
                        child.set_cow_state(vma.start(), CowState::CopyOnWrite)?;
                    }

                    // Rebind parent store + set COW flags
                    if let Some(ps) = parent_shadow {
                        self.rebind_store(vma.start(), ps)?;
                        self.set_cow_state(vma.start(), CowState::CopyOnWrite)?;
                    }
                }
                VmInherit::Share => {
                    child.map(
                        VirtAddr(vma.start()), VirtAddr(vma.end()),
                        vma.protection, vma.mapping.clone(),
                    )?;
                }
                VmInherit::None => {}
            }
        }

        // Phase 2: Pmap-level work using container API
        let child_pmap_arc = child.pmap.clone();
        let mut child_pmap = child_pmap_arc.lock();
        let parent_pmap_arc = self.pmap.clone();
        let mut parent_pmap = parent_pmap_arc.lock();

        let mut batch = ShootdownBatch::new(&parent_pmap);
        for vma in self.iter() {
            if !vma.protection.contains(MapPerm::W) {
                // RO: copy parent pages into child as-is
                for (va, pa, _flags) in parent_pmap.range(
                    VirtAddr(vma.start()), VirtAddr(vma.end()))
                {
                    child_pmap.entry_or_insert(va, pa, vma.protection).ok();
                }
            } else {
                // Writable: downgrade parent RO, enter child RO
                let ro = vma.protection & !MapPerm::W;
                parent_pmap.for_each_in_range_mut(
                    VirtAddr(vma.start()), VirtAddr(vma.end()),
                    |mut entry| {
                        let pa = entry.pa();
                        child_pmap.entry_or_insert(entry.va(), pa, ro).ok();
                        batch.add(entry.set_perm(ro));
                    },
                );
            }
        }
        // batch drops → local flush + release remote harts
        Ok(())
    }
}
```

Sigcode is no longer special-cased in fork. It has a VMA with
`VmMapping::Shared` and `VmInherit::Share`, so `cow_fork_into` handles
it via the `Share` branch — Arc::clone the object, copy the pmap entry.

## `VmMap::share_into`

For `CLONE_VM` (vfork, threads). Shares the pmap.

```rust
impl VmMap {
    /// Create a VmMap sharing this pmap. For CLONE_VM.
    /// The child VmMap has an empty tree but the same pmap.
    pub fn share_into(&self) -> VmMap {
        VmMap::new(Arc::clone(&self.pmap))
    }
}
```

When `CLONE_VM` is set, the child task's `vm_map` Arc points to the
same `VmMap` as the parent — no new VmMap is created. `share_into` is
only needed if the child needs its own VmMap metadata but shared page
tables. For full CLONE_VM (Linux model), `Arc::clone` of the parent's
`vm_map` field is sufficient.

## Task Struct Changes

Resources that can be shared need Arc wrapping:

```rust
pub struct Task {
    // Shared (Arc-wrapped)
    pub vm_map: Arc<LeveledRwLock<VmMap, 1>>,
    pub fd_table: Arc<Mutex<FdTable, 4>>,
    pub signal_actions: Arc<Mutex<[SigAction; NSIG], 4>>,

    // Per-task (never shared)
    pub pid: u32,
    pub parent: Weak<Task>,
    pub children: Mutex<Vec<Arc<Task>>, 4>,
    pub trap_frame: Mutex<TrapFrame, 4>,
    kstack_base: PhysAddr,
    pub kernel_sp: usize,
    pub brk: AtomicUsize,
    pub cwd: Mutex<String, 4>,
    state: AtomicU8,
    pub exit_status: AtomicI32,
    pub parent_waker: Mutex<Option<Waker>, 4>,
    pub pgid: AtomicU32,
    pub top_level_waker: Mutex<Option<Waker>, 4>,
    pub signals_pending: AtomicU64,
    pub signals_blocked: AtomicU64,

    // Vfork
    pub vfork_done: Option<Arc<VforkDone>>,
}
```

Arc wrapping adds one indirection per access. For non-shared cases
(traditional fork), the refcount is 1 — negligible cost.

Helper methods for sharing:

```rust
impl Task {
    pub fn vm_map_arc(&self) -> Arc<LeveledRwLock<VmMap, 1>> {
        Arc::clone(&self.vm_map)
    }
    pub fn share_fd_table(&self, parent: &Task) {
        // replace self.fd_table with Arc::clone of parent's
    }
    pub fn share_signal_actions(&self, parent: &Task) {
        // replace self.signal_actions with Arc::clone of parent's
    }
}
```

## VforkDone

Parent blocks after spawn until child exits or execs.

```rust
pub struct VforkDone {
    done: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl VforkDone {
    pub fn signal(&self) { ... }   // called by child on exit/exec
    pub async fn wait(&self) { ... }  // awaited by parent in sys_clone
}
```

## File Renames

`proc/fork.rs` → `proc/clone.rs`:
- `fork()` → `do_clone()`
- `cow_fork_vm()` → `VmMap::cow_fork_into()` (moves to `mm/vm/map/mod.rs`)
- `deep_copy_pages()` → removed (or feature-gated wrapper calling
  `cow_fork_into` with full-copy VmObjects)

## Files

| File | Change |
|------|--------|
| `proc/clone.rs` (new, replaces `fork.rs`) | `do_clone()`, `CloneFlags`, `VforkDone` |
| `proc/task.rs` | Arc-wrap `vm_map`, `fd_table`, `signal_actions`. Add `vfork_done`. |
| `mm/vm/map/mod.rs` | Add `cow_fork_into()`, `share_into()` |
| `syscall/process.rs` | `sys_clone` → parse flags → `do_clone()` → spawn → vfork block |
| `proc/fork.rs` | Deleted (replaced by `clone.rs`) |
