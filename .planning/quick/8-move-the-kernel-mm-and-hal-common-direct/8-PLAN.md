---
phase: quick-8
plan: 01
type: execute
wave: 1
depends_on: []
files_modified:
  - Cargo.toml
  - kernel/Cargo.toml
  - kernel/src/mm/mod.rs
  - kernel/src/mm/allocator/mod.rs
  - kernel/src/mm/pmap/mod.rs
  - kernel/src/mm/vm/mod.rs
autonomous: true
requirements: []

must_haves:
  truths:
    - kernel-mm and hal-common root directories no longer exist
    - kernel builds successfully without external path dependencies
    - kernel/src/mm contains all kernel-mm functionality
  artifacts:
    - path: "Cargo.toml"
      provides: "Workspace with only kernel member"
      contains: 'members = ["kernel"]'
    - path: "kernel/src/mm/mod.rs"
      provides: "Complete mm module with all submodules"
      min_lines: 5
  key_links:
    - from: "kernel/src/mm/"
      to: "crate::hal_common"
      via: "internal module imports"
      pattern: "use crate::hal_common::"
---

<objective>
Complete the workspace consolidation by moving kernel-mm into kernel/src/mm and removing both root-level hal-common and kernel-mm directories.

Purpose: Simplify workspace structure, eliminate all external path dependencies, make kernel fully self-contained
Output: Single-crate workspace with kernel as internal module structure
</objective>

<execution_context>
@/Users/3y/.claude/get-shit-done/workflows/execute-plan.md
@/Users/3y/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/STATE.md
@.planning/quick/7-move-hal-common-and-kernel-mm-into-kerne/7-SUMMARY.md

Quick task 7 moved hal-common into kernel/src/hal_common but kept root directories because kernel-mm still depended on hal-common as external crate. This task completes the consolidation by moving kernel-mm content into kernel/src/mm and removing both root directories.

Current state:
- kernel/src/hal_common/ exists (moved in quick-7)
- kernel/src/mm/ exists with some content
- kernel-mm/ at root still exists with lib.rs and test infrastructure
- hal-common/ at root still exists (kept for kernel-mm dependency)
- Workspace members: ["kernel", "kernel-mm"]
</context>

<tasks>

<task type="auto">
  <name>Task 1: Consolidate kernel-mm content into kernel/src/mm</name>
  <files>
    kernel/src/mm/mod.rs
    kernel/src/mm/allocator/mod.rs
    kernel/src/mm/pmap/mod.rs
    kernel/src/mm/vm/mod.rs
    kernel/src/mm/uio.rs
  </files>
  <action>
    Merge kernel-mm/src/ content into kernel/src/mm/:

    1. Compare kernel-mm/src/lib.rs with kernel/src/mm/mod.rs - ensure all module declarations present
    2. Check for any test infrastructure in kernel-mm/src/lib.rs (kprintln/klog macros, pmap_tests module) - these are test-only, skip them
    3. Verify all submodules (allocator, pmap, uio, vm) are already in kernel/src/mm/
    4. Update any remaining hal_common:: imports in kernel/src/mm/ to use crate::hal_common::
    5. Ensure kernel/src/mm/mod.rs has proper module structure (no lib.rs artifacts like #![no_std])

    Do NOT copy test infrastructure (kprintln/klog dummy macros, pmap_tests module) - kernel has real implementations.
  </action>
  <verify>
    <automated>cargo build --release -p kernel --target riscv64gc-unknown-none-elf</automated>
  </verify>
  <done>kernel/src/mm/ contains all production code from kernel-mm, kernel compiles successfully</done>
</task>

<task type="auto">
  <name>Task 2: Remove root directories and update workspace</name>
  <files>
    Cargo.toml
    kernel/Cargo.toml
  </files>
  <action>
    Clean up workspace structure:

    1. Update root Cargo.toml:
       - Change members from ["kernel", "kernel-mm"] to ["kernel"]
       - Keep exclude = ["user"] and resolver = "2"
       - Keep workspace.dependencies section unchanged

    2. Verify kernel/Cargo.toml has no hal-common or kernel-mm dependencies (should already be clean from quick-7)

    3. Remove root-level directories:
       - rm -rf hal-common/
       - rm -rf kernel-mm/

    4. Verify no references to hal-common or kernel-mm as external crates remain in any Cargo.toml
  </action>
  <verify>
    <automated>cargo build --release -p kernel --target riscv64gc-unknown-none-elf && [ ! -d hal-common ] && [ ! -d kernel-mm ]</automated>
  </verify>
  <done>Workspace contains only kernel member, root hal-common and kernel-mm directories deleted, kernel builds successfully</done>
</task>

<task type="auto">
  <name>Task 3: Update Makefile and verify build system</name>
  <files>
    Makefile
  </files>
  <action>
    Update Makefile to reflect new workspace structure:

    1. Check if Makefile has any references to hal-common or kernel-mm targets
    2. Update test target if it references kernel-mm (should test kernel only now)
    3. Verify test-all target works with new structure
    4. Run full build verification:
       - make kernel-rv64 (kernel binary)
       - make test (host unit tests)

    The Makefile was already updated in quick-7 to test kernel-mm instead of hal-common. Now that kernel-mm is gone, update to test kernel only.
  </action>
  <verify>
    <automated>make kernel-rv64 && make test</automated>
  </verify>
  <done>Makefile updated, kernel builds successfully, host tests pass</done>
</task>

</tasks>

<verification>
Final checks:
- [ ] cargo build --release -p kernel --target riscv64gc-unknown-none-elf succeeds
- [ ] hal-common/ directory does not exist at root
- [ ] kernel-mm/ directory does not exist at root
- [ ] Workspace members = ["kernel"] only
- [ ] make kernel-rv64 succeeds
- [ ] make test succeeds (host tests)
- [ ] No external path dependencies in any Cargo.toml
</verification>

<success_criteria>
- Workspace simplified to single kernel crate
- All mm functionality accessible via kernel/src/mm/
- All hal_common functionality accessible via kernel/src/hal_common/
- No root-level hal-common or kernel-mm directories
- Kernel builds and tests pass
- No external path dependencies remain
</success_criteria>

<output>
After completion, create `.planning/quick/8-move-the-kernel-mm-and-hal-common-direct/8-SUMMARY.md`
</output>
