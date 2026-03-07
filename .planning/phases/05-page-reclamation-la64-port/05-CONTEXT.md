# Phase 5: BSD VM Model Adherence - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

Refactor VM subsystem to adhere to BSD model: implement proper COW with shadow chain walk and page-grab logic, consolidate exec into unified do_execve using ExecContext from elf.rs, and unify page cache access through VmObject methods with proper busy lock protocol. This phase focuses on correctness and BSD architectural alignment, deferring page reclamation and LA64 port to Phase 6.

</domain>

<decisions>
## Implementation Decisions

### COW Mechanics
- Fault-driven COW (BSD standard vm_fault pattern): write fault on read-only PTE → check if page is COW → grab/copy → remap writable
- Shadow chain walk for page-grab: walk from child to parent, grab first page found (BSD vm_object_page_grab pattern)
- shadow_count tracking: increment on fork (child points to parent), decrement on Drop. Used by future collapse to determine when page migration is safe (BSD vm_object_collapse semantics)
- Fork transition: replace current hard-copy with COW shadow chain. Keep hard-copy code as `#[cfg(feature = "fork-hardcopy")]` fallback for testing/debugging

### exec Consolidation
- Unified do_execve(path, argv, envp) replaces current exec/exec_with_args split
- elf.rs provides ExecContext::build() that parses ELF headers and returns load segments in ExecContext struct
- exec.rs (do_execve) calls ExecContext::build(), then maps each region from mapped_regions into VmMap
- ExecContext contains: load_base, mapped_regions (Vec<MemoryRegion>), brk, user_entry, interp_entry, interp_path, phdr_vaddr, phnum, phentsize
- Clean separation: elf.rs handles parsing, exec.rs handles VmMap operations and allocation

### Page Cache Unification
- All kernel code accesses page cache through VmObject methods, not manual page pull/insert
- VmObject::lookup_page(offset) → Arc<VmPage> with sBusy held (read-only access, BSD vm_page_lookup pattern)
- VmObject::grab_page(offset) → Arc<VmPage> with exBusy held (read-write access, BSD vm_page_grab pattern)
- Caller responsible for releasing busy locks (release_steady_state() for sBusy, release_exclusive() for exBusy)
- VmObject::insert_page(offset, page) for new pages
- VmObject::remove_page(offset) for eviction/teardown

### Claude's Discretion
- Exact COW fault handler flow (check shadow chain depth, optimization for common case)
- Error handling for exec failures (cleanup strategy, partial state rollback)
- VmObject method implementation details (locking granularity, optimization for hot paths)
- Testing strategy for COW vs hard-copy fallback

</decisions>

<code_context>
## Existing Code Insights

### Reusable Assets
- VmPage with exBusy/sBusy lock states (kernel/src/mm/vm/page.rs): readers-writer busy lock for cross-thread sync, WANTED flag for sleepers, downgrade_exclusive_to_shared() for lock downgrade
- VmObject with shadow chain support (kernel/src/mm/vm/vm_object.rs): pages BTreeMap, backing pointer, shadow_count field, iterative Drop for deep chains
- ExecContext struct in elf.rs (kernel/src/proc/elf.rs): already has build() method, MemoryRegion with vaddr/memsz/filesz/offset/prot, interp_path handling

### Established Patterns
- BSD pcb_onfault for exception fixup (Phase 1): one copy primitive (uiomove), O(1) trap handler check
- IrqSafeSpinLock for all shared data structures (Phase 1): prevents IRQ-reentrance deadlock
- Persistent task future model (Phase 1): fast syscalls complete in one poll, no per-syscall spawn
- Iterative Drop for deep shadow chains (Phase 2): handles 500+ depth without stack overflow

### Integration Points
- vm_fault handler (kernel/src/mm/vm/fault.rs or fault_async.rs): needs COW path for write faults on read-only PTEs
- fork implementation (kernel/src/proc/fork.rs): currently does hard-copy, needs to create shadow chain instead
- exec implementation (kernel/src/proc/exec.rs): currently has exec/exec_with_args split, needs unified do_execve
- Page cache users (fs/delegate.rs, fs/vnode.rs, etc.): currently manual page pull/insert, need to use VmObject methods

</code_context>

<specifics>
## Specific Ideas

- VmPage busy lock protocol follows FreeBSD's vm_page.c: exBusy for exclusive access (I/O, destruction, identity change), sBusy for shared read-only access, WANTED flag for wakeup coordination
- ExecContext::build() already exists in elf.rs with goblin parsing, just needs do_execve to consume it properly
- Fork hard-copy fallback kept as compile-time feature flag for regression testing and debugging COW issues

</specifics>

<deferred>
## Deferred Ideas

- Page reclamation (page daemon, writeback daemon, shadow collapse) — moved to Phase 6
- LA64 architecture port (4-level page tables, negative PTE logic) — moved to Phase 6
- Swap daemon and anonymous page pressure handling — out of scope per PROJECT.md

</deferred>

---

*Phase: 05-bsd-vm-model-adherence*
*Context gathered: 2026-03-08*
