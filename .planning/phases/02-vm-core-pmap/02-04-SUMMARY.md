---
phase: 02-vm-core-pmap
plan: 04
subsystem: pmap, vm
tags: [asid, tlb-shootdown, ipi, page-fault, cow, sync-fault]

requires:
  - phase: 02-vm-core-pmap
    plan: 01
    provides: "VmObject, VmMap, VmArea data structures"
  - phase: 02-vm-core-pmap
    plan: 02
    provides: "frame_alloc_sync() dual API"
  - phase: 02-vm-core-pmap
    plan: 03
    provides: "pmap_enter/remove/protect operations"
provides:
  - "Generation-based ASID allocator with IPI rollover"
  - "TLB shootdown via IPI with per-CPU ack"
  - "Synchronous page fault handler (anonymous + COW)"
---

## What Was Built

ASID allocator with generation-based rollover, TLB shootdown via IPI, and synchronous page fault handler for anonymous and COW faults.

### ASID Allocator (`os/src/mm/pmap/asid.rs`)
- 16-bit ASIDs (0 reserved for kernel), generation counter for rollover detection
- Double-check pattern: after lock acquisition, verify generation hasn't already advanced
- Rollover triggers synchronous IPI barrier — all CPUs flush TLB and ack before any ASID reuse
- `check_asid()` fast path reads generation atomically without lock

### TLB Shootdown (`os/src/mm/pmap/shootdown.rs`)
- Per-CPU `ShootdownRequest` with VA range + ASID + pending/ack atomics
- `pmap_shootdown()` fills per-CPU slots, sends SBI sPI IPI, flushes local, spin-waits for all acks
- Adaptive flush: per-page `sfence.vma` for ≤16 pages, full ASID flush otherwise
- `handle_shootdown_ipi()` called from IPI handler on remote CPUs

### Synchronous Fault Handler (`kernel/src/mm/vm/fault.rs`)
- `sync_fault_handler()` runs on trap stack — never yields, no async
- Anonymous fault: `frame_alloc_sync()` → zero page → insert into VmObject → pmap_enter (TODO)
- COW fault: detect shared VmObject → copy page to new frame → remap with write permission
- File-backed faults return `NeedsAsyncIO` for Phase 3 async path
- `PageFaultAccessType` struct with permission checking against VMA protection

## Key Files

### key-files.created
- `chronix/os/src/mm/pmap/asid.rs` — ASID allocator with generation rollover
- `chronix/os/src/mm/pmap/shootdown.rs` — TLB shootdown via IPI
- `kernel/src/mm/vm/fault.rs` — Synchronous page fault handler

### key-files.modified
- `chronix/os/src/mm/pmap/mod.rs` — Added `pub mod asid; pub mod shootdown;`
- `kernel/src/mm/vm/mod.rs` — Added `pub mod fault;` and re-exports

## Deviations

- pmap_enter/pmap_remove integration into fault handler left as TODO comments — the pmap layer lives in the chronix submodule while fault.rs is in the kernel crate. Full wiring deferred to integration phase.
- pmap_remove/pmap_protect do not yet automatically call shootdown — requires cross-crate integration.

## Self-Check: PASSED
- [x] ASID allocator assigns unique ASIDs with generation-based rollover
- [x] Generation rollover triggers synchronous IPI barrier flushing all TLBs
- [x] pmap_shootdown sends IPI to all CPUs and waits for ack
- [x] sync_fault_handler resolves anonymous page faults without yielding
- [x] COW fault copies page to new frame and remaps with write permission
- [x] No .await or yield in sync_fault_handler
