# EH-2: `kerr!` Macro + FaultError Elimination — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `kerr!`/`kreturn!` logging macros that enforce errno+log pairing, then eliminate the `FaultError` domain type by converting all fault sites to return `Errno` via `kerr!`.

**Architecture:** `kerr!(mod, level, errno, fmt, args...)` is a macro that logs via `klog!` and evaluates to the errno value. `kreturn!` is sugar for `return Err(kerr!(...))`. FaultError is deleted; `FaultResult::Error(Errno)` replaces `FaultResult::Error(FaultError)`. `resolve_user_fault` returns `Result<(), Errno>`. Syscall callers use bare `?`.

**Tech Stack:** Rust `no_std`, `riscv64gc-unknown-none-elf`, existing `klog!` infra

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `kernel/src/console.rs:197` | Modify | Add `kerr!` and `kreturn!` macros after existing `klog!` |
| `kernel/src/mm/vm/fault.rs:14-44` | Modify | Delete `FaultError` enum, update `FaultResult`, add `Errno` import |
| `kernel/src/mm/vm/fault.rs:106-380` | Modify | Convert 11 construction sites from `FaultError::Xyz` to `kerr!(...)` |
| `kernel/src/mm/vm/fault.rs:430-462` | Modify | Update 3 test assertions |
| `kernel/src/mm/vm/fault_async.rs:8-16` | Modify | Update imports: drop `FaultError`, add `Errno` |
| `kernel/src/mm/vm/fault_async.rs:23,132` | Modify | Change return types to `Result<(), Errno>` |
| `kernel/src/mm/vm/fault_async.rs:138-230` | Modify | Convert 7 construction sites to `kerr!(...)` |
| `kernel/src/mm/vm/mod.rs:22` | Modify | Remove `FaultError` from re-export |
| `kernel/src/syscall/fs.rs:1229,1382` | Modify | Drop `.map_err(\|_\| Errno::Efault)`, use bare `?` |
| `kernel/src/hal_common/errno.rs:23` | Modify | Update doc comment (FaultError no longer exists) |
| `reports/01-error-handling.md` | Modify | Mark EH-2 FIXED, update diagram |

---

## Errno Mapping Reference

| Old FaultError variant | New Errno | Log level | Rationale |
|------------------------|-----------|-----------|-----------|
| `OutOfMemory` | `Errno::Enomem` | `error` | Frame/PTE alloc failure — serious, always visible |
| `InvalidAccess` | `Errno::Efault` | `debug` | Linux convention: page-fault permission violation -> EFAULT |
| `NotMapped` | `Errno::Efault` | `debug`/`warn` | No VMA at address |
| `IoError` | `Errno::Eio` | `error` | Pager I/O failure — serious |

---

## Chunk 1: Macros + FaultError Elimination

### Task 1: Add `kerr!` and `kreturn!` macros

**Files:**
- Modify: `kernel/src/console.rs:197` (insert after `klog!` macro, before `_klog_if!`)

- [ ] **Step 1: Add `kerr!` macro after klog!**
- [ ] **Step 2: Add `kreturn!` macro after kerr!**
- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit** — `feat(log): add kerr! and kreturn! macros for errno+log pairing`

### Task 2: Eliminate FaultError in fault.rs

**Files:**
- Modify: `kernel/src/mm/vm/fault.rs`

- [ ] **Step 1: Add `Errno` to imports**
- [ ] **Step 2: Update FaultResult::Error payload from FaultError to Errno, delete FaultError enum**
- [ ] **Step 3: Convert 8 sites that had preceding klog! — merge into kerr!**
- [ ] **Step 4: Convert 3 bare sites — add kerr! where no klog! existed**
- [ ] **Step 5: Update 3 test assertions**

### Task 3: Eliminate FaultError in fault_async.rs

**Files:**
- Modify: `kernel/src/mm/vm/fault_async.rs`

- [ ] **Step 1: Update imports — drop FaultError, add Errno**
- [ ] **Step 2: Update function signatures to Result<(), Errno>**
- [ ] **Step 3: Convert 7 construction sites to kerr!**

### Task 4: Update re-exports and consumers

**Files:**
- Modify: `kernel/src/mm/vm/mod.rs:22`
- Modify: `kernel/src/syscall/fs.rs:1229,1382`

- [ ] **Step 1: Remove FaultError from vm/mod.rs re-export**
- [ ] **Step 2: Drop lossy map_err in syscall/fs.rs (2 sites)**
- [ ] **Step 3: Build** — `make kernel-rv64`
- [ ] **Step 4: Commit Tasks 2-4 together**

### Task 5: Update documentation

**Files:**
- Modify: `kernel/src/hal_common/errno.rs:23-25`
- Modify: `reports/01-error-handling.md`

- [ ] **Step 1: Update errno.rs doc comment**
- [ ] **Step 2: Mark EH-2 FIXED in report**
- [ ] **Step 3: Commit** — `docs: mark EH-2 fixed, document kerr! convention`

### Task 6: Integration test

- [ ] **Step 1: Smoke test** — `make agent-test`
- [ ] **Step 2: Full integration test** — `make python-test-rv64`
