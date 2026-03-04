---
phase: quick-4
plan: 01
subsystem: vm
tags: [refactoring, type-safety, compilation-fix]
dependency_graph:
  requires: [quick-3]
  provides: [VirtPageNum-export, refactored-vm-interface]
  affects: [hal-common, kernel-vm, kernel-proc, kernel-executor]
tech_stack:
  added: []
  patterns: [newtype-pattern, affine-space-arithmetic]
key_files:
  created: []
  modified:
    - hal-common/src/lib.rs
    - kernel/src/mm/vm/vm_map.rs
    - kernel/src/mm/vm/vm_object.rs
    - kernel/src/mm/vm/fault.rs
    - kernel/src/executor/user_task.rs
    - kernel/src/proc/exec.rs
    - kernel/src/proc/fork.rs
    - kernel/src/mm/vm/test_integration.rs
decisions:
  - Export VirtPageNum from hal-common for VM subsystem usage
  - VirtPageNum uses affine space arithmetic (+ usize, not + VirtPageNum)
  - File offset arithmetic kept in usize, cast to u64 only for delegate calls
  - Replace translate_to_obj_index/translate_to_obj_page_offset with pindex_for
metrics:
  duration: 788s
  tasks_completed: 3
  files_modified: 8
  commits: 3
  completed_date: "2026-03-04"
---

# Quick Task 4: Use Refactored VM Interface

**One-liner:** Fixed compilation errors from quick-3 VM refactoring by exporting VirtPageNum and adapting all usage sites to newtype pattern and refactored interface methods.

## Objective

Fix compilation errors after VM subsystem refactoring by adding missing VirtPageNum type export and updating all usage sites to use the refactored VmArea interface methods.

## Tasks Completed

### Task 1: Add VirtPageNum to hal-common and fix VM imports
- **Status:** ✅ Complete
- **Commit:** c76afdc
- **Changes:**
  - Exported VirtPageNum from hal-common/src/lib.rs
  - Removed unused `Ordering` import from vm_object.rs
  - Removed unused `self` import from vm_map.rs

### Task 2: Fix user_task.rs to use refactored VM interface
- **Status:** ✅ Complete
- **Commit:** a301346
- **Changes:**
  - Imported VirtPageNum in user_task.rs
  - Replaced `translate_to_obj_index` with `pindex_for` method
  - Fixed file_offset arithmetic to use consistent usize types
  - Cast to u64 only when calling page_cache_fetch_by_id
  - Wrapped integer literals with VirtPageNum() constructor
  - Replaced `translate_to_obj_page_offset` with `pindex_for` in fault.rs

### Task 3: Verify kernel builds successfully
- **Status:** ✅ Complete
- **Commit:** 28ca95d (deviation fixes)
- **Result:** Kernel compiles with 0 errors, 139 warnings

## Deviations from Plan

### Auto-fixed Issues (Rule 1 - Bugs)

**1. [Rule 1 - Bug] VirtPageNum newtype adaptation in proc/exec.rs**
- **Found during:** Task 3 verification
- **Issue:** VirtPageNum changed from type alias to newtype, requiring constructor calls and arithmetic fixes
- **Fix:**
  - Imported VirtPageNum
  - Wrapped integer literals with VirtPageNum(0)
  - Fixed affine space arithmetic (VirtPageNum + usize, not + VirtPageNum)
  - Cast file_size_in_vma to usize
  - Removed const from va_end (next_page_head not const)
- **Files modified:** kernel/src/proc/exec.rs
- **Commit:** 28ca95d

**2. [Rule 1 - Bug] VirtPageNum newtype adaptation in proc/fork.rs**
- **Found during:** Task 3 verification
- **Issue:** Same VirtPageNum newtype issues in fork.rs
- **Fix:**
  - Imported VirtPageNum
  - Fixed obj_offset calculation to use affine space arithmetic
- **Files modified:** kernel/src/proc/fork.rs
- **Commit:** 28ca95d

**3. [Rule 1 - Bug] VirtPageNum newtype adaptation in test_integration.rs**
- **Found during:** Task 3 verification
- **Issue:** Test code using integer literals where VirtPageNum expected
- **Fix:**
  - Imported VirtPageNum
  - Wrapped all integer literals with VirtPageNum()
  - Replaced remove_page (not implemented) with truncate_pages
- **Files modified:** kernel/src/mm/vm/test_integration.rs
- **Commit:** 28ca95d

## Verification

All success criteria met:
- ✅ `cargo check --target riscv64gc-unknown-none-elf -p kernel` completes with 0 errors
- ✅ VirtPageNum type available from hal_common::addr
- ✅ user_task.rs uses VmArea::pindex_for() method
- ✅ File offset calculations are type-consistent (usize throughout, cast to u64 for delegate)
- ✅ All proc and test files adapted to VirtPageNum newtype

## Key Decisions

1. **VirtPageNum affine space arithmetic:** VirtPageNum + usize returns VirtPageNum, but VirtPageNum + VirtPageNum is not supported. Use `obj_offset + delta` pattern.

2. **File offset type consistency:** Keep file_offset as usize in VmArea, cast to u64 only when calling delegate functions that expect u64.

3. **Method naming:** Standardized on `pindex_for()` for page index calculation, replacing `translate_to_obj_index` and `translate_to_obj_page_offset`.

## Self-Check: PASSED

**Files created:** None (SUMMARY.md only)

**Files modified:**
- ✅ hal-common/src/lib.rs exists
- ✅ kernel/src/mm/vm/vm_map.rs exists
- ✅ kernel/src/mm/vm/vm_object.rs exists
- ✅ kernel/src/mm/vm/fault.rs exists
- ✅ kernel/src/executor/user_task.rs exists
- ✅ kernel/src/proc/exec.rs exists
- ✅ kernel/src/proc/fork.rs exists
- ✅ kernel/src/mm/vm/test_integration.rs exists

**Commits:**
- ✅ c76afdc exists
- ✅ a301346 exists
- ✅ 28ca95d exists

**Build verification:**
- ✅ Kernel compiles successfully with 0 errors
