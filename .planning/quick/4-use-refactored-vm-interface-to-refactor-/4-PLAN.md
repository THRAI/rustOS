---
phase: quick-4
plan: 01
type: execute
wave: 1
depends_on: []
files_modified:
  - hal-common/src/addr.rs
  - kernel/src/executor/user_task.rs
  - kernel/src/mm/vm/vm_map.rs
  - kernel/src/mm/vm/vm_object.rs
autonomous: true
requirements: []

must_haves:
  truths:
    - "Kernel compiles without errors after VM refactoring"
    - "VirtPageNum type is available from hal-common"
    - "File offset arithmetic uses consistent types"
  artifacts:
    - path: "hal-common/src/addr.rs"
      provides: "VirtPageNum type definition"
      exports: ["VirtPageNum"]
    - path: "kernel/src/executor/user_task.rs"
      provides: "Fixed page fault handler using refactored VM interface"
      min_lines: 500
  key_links:
    - from: "kernel/src/mm/vm/vm_map.rs"
      to: "hal_common::addr::VirtPageNum"
      via: "use statement"
      pattern: "use hal_common::addr::VirtPageNum"
    - from: "kernel/src/executor/user_task.rs"
      to: "VmArea::pindex_for"
      via: "method call"
      pattern: "vma\\.pindex_for"
---

<objective>
Fix compilation errors after VM subsystem refactoring by adding missing VirtPageNum type and updating usage sites.

Purpose: The quick-3 refactoring introduced VirtPageNum and new VmArea methods but didn't update all usage sites or add the type to hal-common.
Output: Kernel compiles successfully with refactored VM interface.
</objective>

<execution_context>
@/Users/3y/.claude/get-shit-done/workflows/execute-plan.md
@/Users/3y/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/STATE.md
@.planning/quick/3-refactor-kernel-vm-submodule-based-on-ai/3-SUMMARY.md

# Refactoring introduced:
- VirtPageNum type (used but not defined in hal-common)
- VmArea::pindex_for() method for page index calculation
- EntryFlags with COW/NEEDS_COPY/IN_TRANSITION states
- VmObject I/O tracking fields (paging_in_progress, generation)

# Current errors:
1. VirtPageNum not found in hal_common::addr
2. VirtAddr::current_page_head() method missing
3. Type mismatches in file_offset arithmetic (usize vs u64)
4. Unused imports in vm_map.rs and vm_object.rs
</context>

<tasks>

<task type="auto">
  <name>Task 1: Add VirtPageNum to hal-common and fix VM imports</name>
  <files>hal-common/src/addr.rs, kernel/src/mm/vm/vm_map.rs, kernel/src/mm/vm/vm_object.rs</files>
  <action>
Add VirtPageNum type alias to hal-common/src/addr.rs:
- Add `pub type VirtPageNum = usize;` after VirtAddr definition
- This represents page index within a VmObject (not a physical page number)

Clean up unused imports in VM files:
- Remove `use crate::mm::vm::vm_object::self` from vm_map.rs (keep VObjIndex)
- Remove `use core::sync::atomic::Ordering` from vm_object.rs (AtomicU32 methods use inline ordering)
  </action>
  <verify>
    <automated>cargo check --target riscv64gc-unknown-none-elf -p hal-common && cargo check --target riscv64gc-unknown-none-elf -p kernel --message-format=short 2>&1 | grep -E "vm_map.rs|vm_object.rs|VirtPageNum" | head -10</automated>
  </verify>
  <done>VirtPageNum type exists in hal-common, VM files import it successfully, unused import warnings cleared</done>
</task>

<task type="auto">
  <name>Task 2: Fix user_task.rs to use refactored VM interface</name>
  <files>kernel/src/executor/user_task.rs</files>
  <action>
Fix page fault handler and mmap to use refactored interface:

1. Replace `fault_va.current_page_head()` with `fault_va.page_align_down()` (line ~500)
   - current_page_head() doesn't exist, page_align_down() is the correct method

2. Fix file_offset arithmetic type consistency (lines ~570, 588, 597):
   - Change `vma.file_offset + (page_idx * PAGE_SIZE) as u64` to `vma.file_offset + page_idx * PAGE_SIZE`
   - vma.file_offset is usize, keep arithmetic in usize
   - Cast to u64 only when passing to delegate.read_page()

3. Use VmArea::pindex_for() where appropriate:
   - In page fault handler, replace manual `(fault_va.as_usize() - vma.start.as_usize()) / PAGE_SIZE` with `vma.pindex_for(fault_va)`
   - This uses the new refactored interface method
  </action>
  <verify>
    <automated>cargo check --target riscv64gc-unknown-none-elf -p kernel 2>&1 | grep -E "error|warning" | head -20</automated>
  </verify>
  <done>user_task.rs compiles without errors, uses VmArea::pindex_for() for page index calculation, file offset arithmetic is type-consistent</done>
</task>

<task type="auto">
  <name>Task 3: Verify kernel builds successfully</name>
  <files>kernel/src/main.rs</files>
  <action>
Run full kernel build to confirm all VM interface usage is correct:
- cargo check for all errors
- Verify no remaining VirtPageNum import errors
- Verify no type mismatches in VM-related code
- Confirm refactored interface (EntryFlags, pindex_for, I/O tracking fields) compiles cleanly
  </action>
  <verify>
    <automated>cargo check --target riscv64gc-unknown-none-elf -p kernel 2>&1 | tail -5</automated>
  </verify>
  <done>Kernel builds successfully with no errors, refactored VM interface fully integrated</done>
</task>

</tasks>

<verification>
- All VirtPageNum import errors resolved
- user_task.rs page fault handler uses refactored VmArea methods
- File offset arithmetic uses consistent types
- No compilation errors in kernel crate
- Unused import warnings cleared
</verification>

<success_criteria>
- `cargo check --target riscv64gc-unknown-none-elf -p kernel` completes with 0 errors
- VirtPageNum type available from hal_common::addr
- user_task.rs uses VmArea::pindex_for() method
- File offset calculations are type-consistent (usize throughout)
</success_criteria>

<output>
After completion, create `.planning/quick/4-use-refactored-vm-interface-to-refactor-/4-SUMMARY.md`
</output>
