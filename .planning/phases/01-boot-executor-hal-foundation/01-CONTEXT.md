# Phase 1: Boot + Executor + HAL Foundation - Context

**Gathered:** 2026-02-23
**Status:** Ready for planning

<domain>
## Phase Boundary

Kernel boots on rv64 QEMU with a working async executor, SMP, trap infrastructure, and debug tooling. ~3K LOC. This is the foundation — no VM, no VFS, no processes. Just boot, run futures, handle traps, and prove it works with tests.

</domain>

<decisions>
## Implementation Decisions

### Executor Design
- Single FIFO `IrqSafeSpinLock<VecDeque<Runnable>>` per CPU. No priority levels — defer 3-level split to Phase 5 when daemons arrive. The IrqSafeSpinLock encapsulates the inner type so the refactor is a one-line change.
- IrqSafeSpinLock enforced at the type level: `disable_and_save()` before lock, restore on Drop. Every path touching the run queue goes through this wrapper — no bare SpinLock allowed.
- async-task crate for spawn/schedule. `schedule_fn` closure pushes Runnable to target CPU's run queue + sends IPI if cross-CPU.
- Single-layer timer wheel (~256-512 slots, 10ms tick granularity). O(1) insert/expire. Cancellation by marking entries, skip on expiry. No hierarchical wheel — defer if longer timeouts needed.
- Idle loop: `wfi` (wait for interrupt). IPI from cross-CPU wake breaks wfi instantly — zero latency penalty vs spinning.
- Preemption: timer IRQ + `yield_now().await` at trap boundary. Chronix pattern — sufficient for competition. No additional preemption mechanism needed.

### Trap Entry/Exit
- Two separate asm vectors: `__user_trap` (sscratch swap for sp, no satp switch) and `__kernel_trap` (push TrapFrame inline on current kernel stack, no satp switch). stvec rewritten on each user↔kernel transition.
- Shared upper-half kernel page table mapping — kernel always accessible in every process's page table. No satp switch on trap entry/exit.
- Per-CPU trap stack with guard pages between stacks. Trap entry swaps sp with sscratch.
- Full register save: all 32 GPRs + sstatus + sepc + scause + stval. TrapFrame is a Rust struct with MI accessor methods: `frame.pc()`, `frame.set_pc()`, `frame.arg(n)`, `frame.set_arg(n, val)`, `frame.set_ret_val(val)`. MI code never sees raw register indices.
- Single trap vector + Rust dispatch (classify scause). Not vectored mode.
- TrapFrame must include kernel tp (saved/restored so per-CPU data is accessible after trap).

### Project Structure
- Follow BLACKBOX Section 10 workspace layout: kernel/, hal-common/, user/, vendor/, scripts/, firmware/
- Configure Cargo workspace with kernel + hal-common + user members
- Makefile with working targets: `make test` (hal-common host tests), `make run-rv64` (QEMU boot), `make qemu-test-rv64` (Tier 2 integration tests)
- user/ folder with a minimal `hello.S` (for Phase 2 userspace test)
- Vendor Phase 1 deps only: async-task, bitflags, log, buddy_system_allocator. Others added as needed.
- hal-common is pure (no arch-specific code) — compiles on host for Tier 1 tests. Contains PhysAddr, VirtAddr, PageSize, Errno, IrqSafeSpinLock, timer wheel logic.

### Testing Scaffolding
- Tier 1: `cargo test --lib` on hal-common. Test IrqSafeSpinLock, timer wheel, run queue logic. Pure host-side.
- Tier 1.5: loom in hal-common's `[dev-dependencies]`. Concurrency tests for IrqSafeSpinLock (IRQ-reentrance scenario).
- Tier 2: QEMU integration tests via `make qemu-test-rv64`. Scripts in scripts/ to boot QEMU, capture output, check assertions. First Tier 2 test: boot to UART output.
- Register clobber test: verify trap entry/exit saves and restores all 32 GPRs + CSRs correctly.

### SMP Boot Model
- SBI HSM standard: hart 0 boots, initializes everything, then starts secondary harts via `sbi_hart_start()`.
- Per-CPU data: tp register → PerCpu struct for hot path (run queue, current task). Global `[PerCpu; MAX_CPUS]` array (static in .bss) for cross-CPU access (IPI targets).
- FDT discovery + MAX_CPUS=8 cap. Parse /cpus node at boot, build hartid→logical_cpu_id mapping. Harts beyond MAX_CPUS are parked.
- Per-hart 16KB boot stacks allocated in linker script (`boot_stack_bottom` to `boot_stack_top`). Stack address = `boot_stack_top - (hartid * STACK_SIZE)`.
- boot.S: filter hartid >= MAX_CPUS → park. Hart 0 clears .bss. All valid harts compute stack pointer, call `rust_main(hartid, dtb_ptr)`.
- Guard pages between per-CPU stacks (set up after VM init in Phase 2 — for Phase 1, stacks are contiguous in physical memory).

### Claude's Discretion
- Exact timer wheel slot count (256 or 512)
- kprintln! macro implementation details
- GDB stub depth (minimal vs full)
- Exact TrapFrame struct field ordering
- QEMU test harness script implementation

</decisions>

<specifics>
## Specific Ideas

- boot.S and linker script follow the user's detailed spec: `_start` entry, hartid in a0, dtb in a1, per-hart stack calculation, BSS clear by hart 0 only, park excess harts with wfi loop.
- Linker script: BASE_ADDRESS = 0x80200000 (QEMU rv64 virt), sections: .text.entry first, then .text, .rodata, .data, .bss (with sbss/ebss symbols), boot stacks at end.
- Reference BSD for per-CPU data patterns. Reference chronix/delonix for practical tradeoffs.
- "No premature abstraction" — single FIFO queue, single-layer timer wheel, single trap vector. Complexity added only when needed.

</specifics>

<deferred>
## Deferred Ideas

- 3-level priority run queue (realtime > normal > idle) — Phase 5 when daemons arrive
- Hierarchical timer wheel for long timeouts — only if needed
- RISC-V vectored trap mode — only if profiling shows dispatch is hot
- Guard page enforcement between CPU stacks — Phase 2 (requires VM)
- Separate kernel page table (KPTI-style) — post-competition hardening

</deferred>

---

*Phase: 01-boot-executor-hal-foundation*
*Context gathered: 2026-02-23*
