# FDT-Based Platform Discovery — Design Spec

**Date:** 2026-03-13
**Status:** Approved
**Addresses:** DR-2 (hardcoded MMIO addresses) from `reports/07-drivers.md`

## Problem

All peripheral addresses are hardcoded to the QEMU `virt` machine layout:

| Device | Hardcoded Address | File |
|--------|------------------|------|
| UART 16550A | `0x1000_0000` | `hal/rv64/uart.rs:6` |
| PLIC | `0x0C00_0000` | `hal/rv64/plic.rs:6` |
| UART IRQ | `10` | `hal/rv64/plic.rs:9` |
| VirtIO MMIO x8 | `0x10001000..0x10008000` | `drivers/virtio_blk.rs:29-32` |

The kernel cannot run on any RISC-V platform with a different memory map without source changes. The DTB pointer is already passed from OpenSBI via `a1` and used for CPU/memory discovery, but not for peripherals.

## Solution

Replace the 333-line hand-rolled FDT parser (`hal/rv64/fdt.rs`) with the `fdt` crate (v0.1.5, `no_std`, zero dependencies, zero allocations). Parse the DTB once during early boot into a `PlatformInfo` struct. All consumers read addresses from this struct instead of constants.

## Dependency

```toml
fdt = "0.1.5"
```

- `no_std`, no `alloc` required — works before the buddy allocator
- Zero transitive dependencies
- MPL-2.0 license

## Data Structures

```rust
/// A VirtIO MMIO device discovered from the FDT.
#[derive(Debug, Copy, Clone)]
pub struct VirtioMmioDevice {
    pub base: usize,
    pub size: usize,
    pub irq: u32,
}

/// Platform information discovered from FDT.
#[derive(Debug)]
pub struct PlatformInfo {
    // UART
    pub uart_base: usize,
    pub uart_irq: u32,
    // PLIC
    pub plic_base: usize,
    pub plic_size: usize,
    // VirtIO MMIO devices
    pub virtio_mmio: [Option<VirtioMmioDevice>; 8],
    pub virtio_count: usize,
    // Physical memory regions
    pub memory: [MemRegion; 4],
    pub memory_count: usize,
    // CPU topology
    pub hart_to_cpu: [Option<usize>; MAX_HARTS],
    pub cpu_to_hart: [usize; MAX_CPUS],
    pub num_cpus: usize,
}
```

Stored in `static PLATFORM: Once<PlatformInfo>`. Accessed via `pub fn platform() -> &'static PlatformInfo`.

## FDT Parsing Strategy

One function `parse_fdt(dtb_ptr: usize)` populates `PlatformInfo` by querying the `fdt` crate:

| Component | FDT Query | Fallback |
|-----------|-----------|----------|
| CPUs | `fdt.cpus()` iterator, read `reg` for hart IDs | 1 CPU, hart 0 |
| Memory | `fdt.memory().regions()` | 128MB at `0x8000_0000` |
| PLIC | `fdt.find_compatible(&["sifive,plic-1.0.0", "riscv,plic0"])` | `0x0C00_0000` |
| UART | `fdt.find_compatible(&["ns16550a"])` | `0x1000_0000`, IRQ 10 |
| VirtIO MMIO | `fdt.find_all_nodes("/soc/virtio_mmio")` | empty (probe fails gracefully) |

Fallback defaults match current QEMU `virt` values to prevent regressions.

## Boot Sequence Change

```
Before:                          After:
------                           ------
init_uart()      <- hardcoded    init_uart()        <- SBI putchar fallback
parse_cpus(dtb)                  init_heap()
init_heap()                      parse_fdt(dtb)     <- discovers everything
parse_memory(dtb)                reinit_uart()      <- FDT address (if different)
init_plic_hart() <- hardcoded    init_plic_hart()   <- platform().plic_base
init_virtio_blk()<- hardcoded    init_virtio_blk()  <- platform().virtio_mmio
```

`init_uart()` stays before FDT parsing because we need console output for errors. After `parse_fdt()`, `reinit_uart()` switches to the FDT-discovered address (on QEMU virt this is the same `0x1000_0000`, but differs on real hardware).

## Files Changed

| File | Change |
|------|--------|
| `kernel/Cargo.toml` | Add `fdt = "0.1.5"` |
| `kernel/src/hal/rv64/fdt.rs` | Replace hand-rolled parser. Define `PlatformInfo`, `parse_fdt()`, `platform()`. Keep `hart_to_cpu()` / `cpu_to_hart()` API. |
| `kernel/src/hal/rv64/uart.rs` | Remove `const UART_BASE`. Read from `platform().uart_base`. Add `reinit(base)`. |
| `kernel/src/hal/rv64/plic.rs` | Remove `const PLIC_BASE`, `const UART_IRQ`. Read from `platform()`. |
| `kernel/src/drivers/virtio_blk.rs` | Remove `const VIRTIO_MMIO_BASES`. Iterate `platform().virtio_mmio`. |
| `kernel/src/main.rs` | Single `hal::parse_fdt(dtb_ptr)` call. Remove `parse_cpus`, `parse_memory`. |
| `kernel/src/hal/mod.rs` | Export `parse_fdt`, `platform`, `PlatformInfo`. |

## Testing

Existing integration tests (`make qemu-test-rv64`, `make python-test-rv64`) serve as the acceptance test. QEMU virt provides a full FDT — if the kernel boots and all patterns pass, FDT parsing is correct. Fallback defaults ensure no regression.

## Out of Scope

- DR-1 (interrupt-driven VirtIO) — separate work
- DR-3 (concurrent I/O) — separate work
- CLINT/timer discovery — handled by SBI, no MMIO needed
