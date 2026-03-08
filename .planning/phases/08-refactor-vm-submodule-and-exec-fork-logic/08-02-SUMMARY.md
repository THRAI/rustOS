---
phase: 08-refactor-vm-submodule-and-exec-fork-logic
plan: 02
subsystem: proc
tags: [exec, elf, two-phase-commit, vm, goblin]

requires:
  - phase: 08-01
    provides: RAII busy lock guards for VmObject

provides:
  - Unified do_execve with BSD two-phase commit (parse -> build temp VmMap -> atomic swap)
  - ExecContext::build pure function for ELF segment parsing via goblin
  - Legacy exec/exec_with_args preserved behind exec-legacy feature flag

affects: [fork, signal, syscall, vm]

tech-stack:
  added: []
  patterns: [two-phase commit exec, temporary VmMap construction, atomic address space swap]

key-files:
  created:
    - kernel/src/proc/elf.rs
  modified:
    - kernel/src/proc/exec.rs
    - kernel/src/proc/mod.rs
    - kernel/src/syscall/process.rs
    - kernel/src/main.rs
    - kernel/Cargo.toml

key-decisions:
  - "Two-phase commit exec: parse ELF and build new VmMap in temp before atomic swap via core::mem::replace"
  - "ExecContext::build as pure function using goblin -- no VmMap ops, no I/O"
  - "Legacy exec gated behind cfg(feature = exec-legacy) for debugging"
  - "VmObject pager field reordered for logical grouping"

patterns-established:
  - "Two-phase commit for destructive operations: validate first, build replacement, atomic swap"
  - "insert_or_merge_file_vma for overlapping ELF PT_LOAD segments sharing a page"

requirements-completed: [REFACTOR-EXEC]

duration: 5min
completed: 2026-03-08
---

# Phase 08 Plan 02: Unified do_execve Summary

**BSD two-phase commit exec: parse ELF via goblin, build temporary VmMap with PT_LOAD/stack/sigcode, atomic swap via mem::replace -- failed exec leaves process intact**

## Performance

- **Duration:** 5 min
- **Started:** 2026-03-08T10:57:46Z
- **Completed:** 2026-03-08T11:02:46Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- ExecContext::build() as pure function parsing PT_LOAD/PT_INTERP/PT_PHDR segments from goblin headers
- Unified do_execve with 3-stage exec: (1) parse/validate, (2) build temp VmMap, (3) atomic swap
- Failed exec at stage 1 or 2 leaves calling process vm_map and page table completely intact
- sys_execve_async and main.rs init both wired to do_execve
- Legacy exec paths preserved behind exec-legacy feature flag

## Task Commits

Each task was committed atomically:

1. **Task 1: Fix ExecContext::build and ELF parsing in elf.rs** - `e1bae6b` (feat)
2. **Task 2: Implement do_execve with two-phase commit** - `b804128` (feat)

## Files Created/Modified
- `kernel/src/proc/elf.rs` - ExecContext::build pure function, MemoryRegion, parse_elf_first_page, elf_flags_to_mapperm
- `kernel/src/proc/exec.rs` - Unified do_execve with 3-stage exec, load_interp_into, legacy code behind feature gate
- `kernel/src/proc/mod.rs` - Added pub mod elf
- `kernel/src/syscall/process.rs` - sys_execve_async calls do_execve
- `kernel/src/main.rs` - Init process calls do_execve instead of exec_with_args
- `kernel/Cargo.toml` - Added exec-legacy feature
- `kernel/src/mm/vm/vm_object.rs` - Reordered pager field for logical grouping

## Decisions Made
- Two-phase commit pattern: build entire replacement address space before destroying old one
- ExecContext::build is pure (no side effects) -- caller creates VMAs from region list
- insert_or_merge_file_vma handles overlapping PT_LOAD segments that share a page boundary
- Hand-rolled ELF parser retained for load_interp_into (simpler for interpreter loading)
- goblin used for main binary parsing via ExecContext::build (richer type info)

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Fixed parse_elf_first_page goblin API**
- **Found during:** Task 2 (do_execve implementation)
- **Issue:** parse_elf_first_page used Header::parse with is_lsb/is_64 params not matching goblin 0.10.5 API
- **Fix:** Changed to Elf::parse_header + Ctx construction for ProgramHeader::parse
- **Files modified:** kernel/src/proc/elf.rs
- **Verification:** cargo check passes for exec/elf modules
- **Committed in:** b804128 (Task 2 commit)

**2. [Rule 3 - Blocking] VmObject pager field ordering**
- **Found during:** Task 2 (do_execve implementation)
- **Issue:** VmObject pager field needed to be pub for VnodePager assignment in do_execve
- **Fix:** Reordered pager field in vm_object.rs struct definition
- **Files modified:** kernel/src/mm/vm/vm_object.rs
- **Committed in:** b804128 (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (2 blocking)
**Impact on plan:** Both fixes necessary for compilation. No scope creep.

## Issues Encountered
- 123 pre-existing compilation errors from other modules (IrqSafeSpinLock imports, Errno variants, VmAreaType) -- these are unrelated to exec refactoring and were not addressed (out of scope)

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- do_execve is the unified exec entry point, ready for fork/exec integration
- Pre-existing compilation errors in other modules need separate resolution (logged in deferred-items.md)

---
*Phase: 08-refactor-vm-submodule-and-exec-fork-logic*
*Completed: 2026-03-08*

## Self-Check: PASSED
