---
phase: quick-3
plan: 01
type: execute
wave: 1
depends_on: []
files_modified:
  - kernel/src/mm/vm/vm_map.rs
  - kernel/src/mm/vm/vm_object.rs
  - kernel/src/mm/vm/fault.rs
autonomous: true
requirements: []

must_haves:
  truths:
    - VmMap supports RwLock operations with upgrade/downgrade for COW
    - VmMapEntry tracks IN_TRANSITION and NEEDS_COPY flags for concurrent fault handling
    - VmObject uses crossbeam SkipMap for lock-free page lookup
    - VmPage state machine distinguishes exBusy (exclusive) and sBusy (shared) states
    - Pager trait supports clustered I/O (get_pages with slice)
  artifacts:
    - path: "kernel/src/mm/vm/vm_map.rs"
      provides: "VmMap with lock upgrade/downgrade, VmMapEntry with EntryFlags"
      min_lines: 200
    - path: "kernel/src/mm/vm/vm_object.rs"
      provides: "VmObject with SkipMap pages, shadow chain traversal"
      min_lines: 150
    - path: "kernel/src/mm/vm/fault.rs"
      provides: "Multi-stage fault handler with COW materialization"
      min_lines: 300
  key_links:
    - from: "kernel/src/mm/vm/fault.rs"
      to: "VmMap::try_upgrade_read_to_write"
      via: "COW lock upgrade"
      pattern: "try_upgrade.*write"
    - from: "kernel/src/mm/vm/vm_object.rs"
      to: "crossbeam_skiplist::SkipMap"
      via: "lock-free page storage"
      pattern: "SkipMap.*VmPage"
---

<objective>
Refactor kernel vm submodule to align with BSD VM architecture patterns from ai/bsd_vm/vm_fault.md.

Purpose: Establish foundation for multi-stage fault handling (topology routing, COW materialization, data extraction, hardware projection) with proper concurrency primitives (RwLock upgrade/downgrade, busy states, shadow chain traversal).

Output: Enhanced vm_map.rs, vm_object.rs, and fault.rs with BSD-style concurrency patterns.
</objective>

<execution_context>
@/Users/3y/.claude/get-shit-done/workflows/execute-plan.md
@/Users/3y/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/STATE.md
@ai/bsd_vm/vm_fault.md
@kernel/src/mm/vm/vm_map.rs
@kernel/src/mm/vm/vm_object.rs
@kernel/src/mm/vm/fault.rs
</context>

<tasks>

<task type="auto">
  <name>Task 1: Add VmMap lock primitives and VmMapEntry flags</name>
  <files>kernel/src/mm/vm/vm_map.rs</files>
  <action>
Add to VmMap struct:
- `timestamp: AtomicU32` for ABA defense
- Helper methods: `try_upgrade_read_to_write() -> bool`, `downgrade_write_to_read()`

Add EntryFlags bitflags to VmMapEntry:
- `COW`, `NEEDS_COPY`, `IN_TRANSITION`, `NEEDS_WAKEUP`, `GROWS_DOWN`

Add VmMapEntry methods:
- `pub fn pindex_for(&self, vaddr: VirtAddr) -> VirtPageNum` — coordinate transform
- `pub fn is_in_transition(&self) -> bool` — check IN_TRANSITION flag
- `pub fn needs_copy(&self) -> bool` — check NEEDS_COPY flag
- `pub fn clear_needs_copy(&mut self)` — clear flag (requires write lock)

Keep existing BTreeMap-based VmMap structure. Do NOT replace with Splay tree (future work). Add lock upgrade/downgrade as no-op stubs that return true/void (RwLock upgrade is not natively supported in parking_lot, document this limitation).
  </action>
  <verify>
    <automated>cd /Users/3y/playground/c/freebsd && cargo check --target riscv64gc-unknown-none-elf -p kernel 2>&1 | grep -E "(error|warning.*vm_map)" | head -20</automated>
  </verify>
  <done>VmMap has timestamp field, lock helper stubs, VmMapEntry has EntryFlags and coordinate transform methods</done>
</task>

<task type="auto">
  <name>Task 2: Refactor VmObject to use SkipMap and add shadow traversal</name>
  <files>kernel/src/mm/vm/vm_object.rs</files>
  <action>
Replace `pages: BTreeMap<VObjIndex, OwnedPage>` with `pages: Arc<crossbeam_skiplist::SkipMap<u64, Arc<OwnedPage>>>`.

Add to VmObject:
- `paging_in_progress: AtomicU32` — I/O barrier
- `generation: AtomicU32`, `clean_generation: AtomicU32` — dirty tracking

Add methods:
- `pub fn backing_object(&self) -> Option<Arc<RwLock<VmObject>>>` — return backing clone
- `pub fn lookup_page_in_chain(&self, pindex: VirtPageNum) -> Option<PhysAddr>` — traverse shadow chain until page found or chain ends

Update existing methods to use SkipMap API:
- `lookup_page()` uses `pages.get(&pindex.0)`
- `insert_page()` uses `pages.insert(pindex.0, Arc::new(page))`
- `has_page()` uses `pages.contains_key(&pindex)`

Keep resident_count as field (SkipMap::len() is O(n)). Increment/decrement on insert/remove.

Add crossbeam-skiplist = "0.1" to kernel/Cargo.toml dependencies (default-features = false).
  </action>
  <verify>
    <automated>cd /Users/3y/playground/c/freebsd && cargo check --target riscv64gc-unknown-none-elf -p kernel 2>&1 | grep -E "(error|warning.*vm_object)" | head -20</automated>
  </verify>
  <done>VmObject uses SkipMap for pages, has paging_in_progress/generation fields, backing_object() and lookup_page_in_chain() methods exist</done>
</task>

<task type="auto">
  <name>Task 3: Enhance fault.rs with multi-stage fault comments and busy state placeholders</name>
  <files>kernel/src/mm/vm/fault.rs</files>
  <action>
Add structured comments to sync_fault_handler documenting the 4-stage BSD fault flow:
```rust
// Stage 1: Topology Routing (VmMap read lock, lookup VMA, check IN_TRANSITION)
// Stage 2: COW Materialization (try_upgrade to write lock, shadow(), downgrade)
// Stage 3: Data Extraction (grab page with exBusy, traverse backing, copy)
// Stage 4: Hardware Projection (downgrade exBusy to sBusy, pmap_enter, release)
```

Add VmPage busy state placeholder types (no implementation, just type definitions):
```rust
/// Placeholder for VmPage busy lock states (future: exBusy/sBusy state machine)
#[derive(Debug, Clone, Copy)]
pub enum BusyState {
    Unbusied,
    ExclusiveBusy,  // exBusy - I/O in progress, identity unstable
    SharedBusy,     // sBusy - stable snapshot for pmap
}
```

In classify_and_handle, add comment before shadow chain lookup:
```rust
// TODO: Traverse shadow chain via lookup_page_in_chain() instead of single-level lookup
```

In handle_cow_fault, add comment before copy:
```rust
// TODO: Acquire exBusy on new page, sBusy on old page, then copy
```

Do NOT change existing logic. Only add comments and placeholder types.
  </action>
  <verify>
    <automated>cd /Users/3y/playground/c/freebsd && cargo check --target riscv64gc-unknown-none-elf -p kernel 2>&1 | grep -E "(error|warning.*fault)" | head -20</automated>
  </verify>
  <done>fault.rs has 4-stage comments, BusyState placeholder type, shadow chain and busy state TODO comments</done>
</task>

</tasks>

<verification>
Run kernel compilation check:
```bash
cd /Users/3y/playground/c/freebsd && make kernel-rv64
```

Verify new types are accessible:
```bash
cd /Users/3y/playground/c/freebsd && cargo test --lib -p kernel --target $(rustc -vV | grep host | cut -d' ' -f2) vm_map 2>&1 | head -30
```
</verification>

<success_criteria>
- kernel compiles without errors for riscv64gc-unknown-none-elf target
- VmMap has timestamp, lock helper stubs, VmMapEntry has EntryFlags
- VmObject uses crossbeam SkipMap for pages storage
- fault.rs has structured 4-stage comments and BusyState placeholder
- Existing tests pass (no behavioral changes, only structural refactoring)
</success_criteria>

<output>
After completion, create `.planning/quick/3-refactor-kernel-vm-submodule-based-on-ai/3-SUMMARY.md`
</output>
