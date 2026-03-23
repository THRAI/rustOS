# LA64 HAL Next Steps Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 rustOS 的 LA64 HAL 从“可编译骨架 + 初步接线”推进到可验证的 M2/M3 基线：稳定内核中断循环 + `write + exit` 最小用户态闭环。

**Architecture:** 严格按 `loongarch64_supplement_plan.md` 的顺序推进：先打通 kernel-only 可运行链路，再做 HAL 边界收口，最后做最小用户态 ABI。共享层只保留 facade 语义，架构差异全部收敛到 `hal/la64/*`。测试以可重复 QEMU 烟测为主，禁止在此阶段扩展到 SMP/full-fs。

**Tech Stack:** Rust `no_std` kernel、`loongarch64-unknown-none`、QEMU `qemu-system-loongarch64`、现有 Makefile/cargo 构建流。

---

## File Structure Map

### 构建与运行门控
- Modify: `kernel/Cargo.toml`（确认并固定 `la64-bringup` 用法）
- Modify: `Makefile`（确保 LA64 kernel-only 不走 ext4 依赖）
- Modify: `kernel/src/main.rs`（保持 LA64 kernel-only 早期门控可观测）

### LA64 最小 runtime
- Modify: `kernel/src/hal/la64/boot.S`
- Modify: `kernel/src/hal/la64/trap.S`
- Modify: `kernel/src/hal/la64/trap.rs`
- Modify: `kernel/src/hal/la64/irq.rs`
- Modify: `kernel/src/hal/la64/time.rs`
- Modify: `kernel/src/hal/la64/ipi.rs`

### MMU/paging
- Modify: `kernel/src/hal/la64/paging/mod.rs`
- Modify: `kernel/src/hal/la64/paging/pte.rs`
- Modify: `kernel/src/hal/la64/paging/walk.rs`
- Modify: `kernel/src/mm/pmap/mod.rs`

### HAL 边界收口
- Modify: `kernel/src/hal/trap.rs`
- Modify: `kernel/src/hal_common/trap_frame.rs`
- Modify: `kernel/src/hal/signal_abi.rs`

### 最小用户态 ABI
- Modify: `kernel/src/hal/la64/syscall_abi.rs`
- Modify: `kernel/src/hal/la64/usercopy.rs`
- Modify: `kernel/src/hal/la64/user_trap.rs`
- Modify: `user/src/syscall.rs`
- Modify: `user/src/bin/initproc.rs`

### 烟测脚本
- Create: `scripts/la64-smoke.sh`

---

## Chunk 1: 先解锁可运行路径（M0->M1）

### Task 1: 固化 LA64 kernel-only 构建路径，避免 ext4 阻塞

**Files:**
- Modify: `kernel/Cargo.toml`
- Modify: `Makefile`

- [ ] **Step 1: 写失败用例（命令层）**

记录当前命令行为：
1) `make ARCH=la64 kernel-la64`
2) `cargo build -p kernel --target loongarch64-unknown-none`

预期至少一个命令因 `loongarch64-linux-musl-cc`（ext4 C 依赖）失败。

- [ ] **Step 2: 先跑失败命令确认基线**

Run: `make ARCH=la64 kernel-la64`
Expected: FAIL，且错误包含 `loongarch64-linux-musl-cc`。

- [ ] **Step 3: 实现最小修复（kernel-only 构建）**

确保 LA64 默认走 `--no-default-features --features la64-bringup`，不拉起 `full-fs`。

- [ ] **Step 4: 复跑验证**

Run: `make ARCH=la64 kernel-la64`
Expected: 不再出现 `loongarch64-linux-musl-cc` 缺失错误；若失败，必须落到内核代码级错误。

- [ ] **Step 5: Commit**

```bash
git add kernel/Cargo.toml Makefile
git commit -m "build(la64): gate kernel bring-up path from ext4 dependency"
```

### Task 2: 固定 boot banner 路径并建立可重复烟测

**Files:**
- Modify: `kernel/src/main.rs`
- Modify: `kernel/src/hal/la64/boot.S`
- Create: `scripts/la64-smoke.sh`

- [ ] **Step 1: 写失败烟测脚本**

脚本检查顺序日志：
1) `hello world`
2) `la64 kernel-only bring-up`
3) `la64 bring-up: trap/timer armed`

- [ ] **Step 2: 运行烟测确认失败**

Run: `bash scripts/la64-smoke.sh`
Expected: FAIL（缺 marker 或超时）。

- [ ] **Step 3: 最小实现修复（只保 boot 可观测）**

保证 `_start -> rust_main -> console` 路径稳定，不引入额外功能。

- [ ] **Step 4: 复跑烟测**

Run: `bash scripts/la64-smoke.sh`
Expected: PASS，3 个 marker 顺序出现。

- [ ] **Step 5: Commit**

```bash
git add kernel/src/main.rs kernel/src/hal/la64/boot.S scripts/la64-smoke.sh
git commit -m "feat(la64): make boot banner path deterministic in kernel-only mode"
```

---

## Chunk 2: 单核 runtime 闭环（M1->M2）

### Task 3: 完成 IRQ save/restore 最小正确性

**Files:**
- Modify: `kernel/src/hal/la64/irq.rs`

- [ ] **Step 1: 写失败检查**

在 bring-up 循环中打印 `irq_enabled` 快照，捕获“关中断后被错误恢复”的情况。

- [ ] **Step 2: 运行失败基线**

Run: `bash scripts/la64-smoke.sh`
Expected: FAIL，出现 IRQ 状态异常或 trap 风暴。

- [ ] **Step 3: 修复 `disable_and_save` / `restore` 语义**

确保仅恢复原始 IE 状态，不引入额外置位副作用。

- [ ] **Step 4: 复跑验证**

Run: `bash scripts/la64-smoke.sh`
Expected: PASS，不再出现 IRQ 状态抖动。

- [ ] **Step 5: Commit**

```bash
git add kernel/src/hal/la64/irq.rs
git commit -m "fix(la64): correct local irq save/restore semantics"
```

### Task 4: 完成 timer rearm + trap decode 基线

**Files:**
- Modify: `kernel/src/hal/la64/time.rs`
- Modify: `kernel/src/hal/la64/trap.rs`
- Modify: `kernel/src/hal/la64/ipi.rs`

- [ ] **Step 1: 写失败检查**

新增 marker：`ticks>=100`、`trap causes(t,s,e,sys)`。

- [ ] **Step 2: 运行失败基线**

Run: `bash scripts/la64-smoke.sh`
Expected: FAIL，无法稳定看到周期 tick 或 cause 分类。

- [ ] **Step 3: 修复 timer 重装与 trap 分发**

分别完成：
1) timer IRQ clear/rearm
2) software/IPI 清 pending
3) 未知异常打印可定位上下文。

- [ ] **Step 4: 复跑验证**

Run: `bash scripts/la64-smoke.sh`
Expected: PASS，至少看到一次 `ticks>=100` 且无 panic 风暴。

- [ ] **Step 5: Commit**

```bash
git add kernel/src/hal/la64/time.rs kernel/src/hal/la64/trap.rs kernel/src/hal/la64/ipi.rs
git commit -m "feat(la64): stabilize timer and trap cause dispatch"
```

### Task 5: 验证 MMU 打开后 kernel-only 仍存活

**Files:**
- Modify: `kernel/src/hal/la64/paging/mod.rs`
- Modify: `kernel/src/hal/la64/paging/pte.rs`
- Modify: `kernel/src/hal/la64/paging/walk.rs`
- Modify: `kernel/src/mm/pmap/mod.rs`

- [ ] **Step 1: 写失败检查**

添加 MMU-on marker：`la64 mmu active`。

- [ ] **Step 2: 跑失败基线**

Run: `make ARCH=la64 run-la64`
Expected: FAIL，MMU 后挂死或立即 fault。

- [ ] **Step 3: 最小修复（只保证 kernel-only）**

修复点按顺序拆分：
1) root 映射最小集合（text/data + UART MMIO）
2) activate/deactivate CSR 设置
3) flush 路径可执行。

- [ ] **Step 4: 复跑验证**

Run: `make ARCH=la64 run-la64`
Expected: 出现 `la64 mmu active` 且 timer 继续增长。

- [ ] **Step 5: Commit**

```bash
git add kernel/src/hal/la64/paging/mod.rs kernel/src/hal/la64/paging/pte.rs kernel/src/hal/la64/paging/walk.rs kernel/src/mm/pmap/mod.rs
git commit -m "feat(la64): make kernel-only mmu path survive runtime"
```

---

## Chunk 3: HAL 边界收口 + 最小用户态闭环（M2->M3）

### Task 6: TrapFrame/Trap facade 去 rv64 语义泄漏

**Files:**
- Modify: `kernel/src/hal_common/trap_frame.rs`
- Modify: `kernel/src/hal/trap.rs`
- Modify: `kernel/src/hal/signal_abi.rs`

- [ ] **Step 1: 写失败检查（编译约束）**

禁止在 LA64 新代码中新增 `sepc/scause/sstatus/stval` 直接字段依赖。

- [ ] **Step 2: 运行编译基线**

Run: `cargo build -p kernel --target loongarch64-unknown-none --no-default-features --features la64-bringup`
Expected: FAIL（若仍有字段直连点）。

- [ ] **Step 3: 修复为语义 API**

统一使用 `pc()/set_pc()/cause_bits()/fault_addr()/status()` 等抽象访问。

- [ ] **Step 4: 复跑验证**

Run: `cargo build -p kernel --target loongarch64-unknown-none --no-default-features --features la64-bringup`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add kernel/src/hal_common/trap_frame.rs kernel/src/hal/trap.rs kernel/src/hal/signal_abi.rs
git commit -m "refactor(hal): enforce semantic trapframe facade for la64"
```

### Task 7: 完成 `write + exit` 最小用户态闭环

**Files:**
- Modify: `kernel/src/hal/la64/syscall_abi.rs`
- Modify: `kernel/src/hal/la64/usercopy.rs`
- Modify: `kernel/src/hal/la64/user_trap.rs`
- Modify: `kernel/src/hal/la64/trap.S`
- Modify: `user/src/syscall.rs`
- Modify: `user/src/bin/initproc.rs`

- [ ] **Step 1: 写失败检查**

定义目标 marker：`la64 user write ok`、`la64 user exit ok`。

- [ ] **Step 2: 跑失败基线**

Run: `make ARCH=la64 run-la64`
Expected: FAIL，缺少至少一个 marker。

- [ ] **Step 3: 分三步最小实现**

1) 对齐 syscall nr/args/ret 寄存器约定。  
2) 对齐 `trap_return` 与 `__user_trap` 的 TrapContext 契约。  
3) 让 usercopy 至少达到“安全但慢”的正确行为。

- [ ] **Step 4: 复跑验证**

Run: `make ARCH=la64 run-la64`
Expected: PASS，用户态最小程序完成一次 `write + exit`。

- [ ] **Step 5: Commit**

```bash
git add kernel/src/hal/la64/syscall_abi.rs kernel/src/hal/la64/usercopy.rs kernel/src/hal/la64/user_trap.rs kernel/src/hal/la64/trap.S user/src/syscall.rs user/src/bin/initproc.rs
git commit -m "feat(la64): close minimal user write-exit round-trip"
```

---

## Final Verification Checklist

- [ ] `cargo build -p kernel`
- [ ] `make ARCH=la64 kernel-la64`
- [ ] `bash scripts/la64-smoke.sh`
- [ ] `cd user && cargo build --target loongarch64-unknown-none`
- [ ] `make ARCH=la64 run-la64`（看到 timer 稳定日志）
- [ ] `make ARCH=la64 run-la64`（看到 `write + exit` 最小闭环 marker）

## Notes

- 严格 YAGNI：本计划不做 SMP 次核启动、不做 full-fs、不做完整 signal 语义。
- 如果任一步失败，必须先收敛到“可复现日志 + 单点修复”，避免并行改动扩大调试面。
- 每个 Task 保持小提交，确保可 bisect。
