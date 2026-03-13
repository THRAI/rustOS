//! FDT (Flattened Device Tree) platform discovery.
//!
//! Parses the DTB once during early boot using the `fdt` crate to discover
//! CPUs, memory regions, PLIC, UART, and VirtIO MMIO devices. Results are
//! stored in a global `PlatformInfo` struct accessed via `platform()`.
//!
//! Replaces the previous hand-rolled FDT parser. All peripheral modules
//! (UART, PLIC, VirtIO-blk) read addresses from `platform()` instead of
//! using hardcoded constants.

use crate::{executor::per_cpu::MAX_CPUS, hal_common::Once};

/// Maximum physical harts we track (for hartid -> cpu_id mapping).
pub const MAX_HARTS: usize = 64;

// ---------------------------------------------------------------------------
// Platform info types
// ---------------------------------------------------------------------------

/// Discovered physical memory region.
#[derive(Debug, Copy, Clone)]
pub struct MemRegion {
    pub base: usize,
    pub size: usize,
}

/// A VirtIO MMIO device discovered from the FDT.
#[derive(Debug, Copy, Clone)]
pub struct VirtioMmioDevice {
    pub base: usize,
    pub size: usize,
    pub irq: u32,
}

/// Platform information discovered from the Flattened Device Tree.
///
/// Populated once via `parse_fdt()` and available for the kernel's lifetime
/// through `platform()`.
pub struct PlatformInfo {
    // UART
    pub uart_base: usize,
    pub uart_irq: u32,
    // PLIC
    pub plic_base: usize,
    pub plic_size: usize,
    // VirtIO MMIO devices (up to 8)
    pub virtio_mmio: [Option<VirtioMmioDevice>; 8],
    pub virtio_count: usize,
    // Physical memory regions (up to 4)
    pub memory: [MemRegion; 4],
    pub memory_count: usize,
    // CPU topology
    pub hart_to_cpu: [Option<usize>; MAX_HARTS],
    pub cpu_to_hart: [usize; MAX_CPUS],
    pub num_cpus: usize,
    // Raw hartid array (for boot_secondary_harts compatibility)
    pub hartids: [usize; MAX_CPUS],
}

// ---------------------------------------------------------------------------
// Global platform info singleton
// ---------------------------------------------------------------------------

static PLATFORM: Once<PlatformInfo> = Once::new();

/// Access the discovered platform information.
///
/// # Panics
/// Panics if called before `parse_fdt()`.
pub fn platform() -> &'static PlatformInfo {
    PLATFORM
        .get()
        .expect("platform() called before parse_fdt()")
}

// ---------------------------------------------------------------------------
// QEMU virt fallback defaults
// ---------------------------------------------------------------------------

const DEFAULT_UART_BASE: usize = 0x1000_0000;
const DEFAULT_UART_IRQ: u32 = 10;
const DEFAULT_PLIC_BASE: usize = 0x0C00_0000;
const DEFAULT_PLIC_SIZE: usize = 0x0400_0000;
const DEFAULT_MEM_BASE: usize = 0x8000_0000;
const DEFAULT_MEM_SIZE: usize = 128 * 1024 * 1024;

// ---------------------------------------------------------------------------
// FDT parsing
// ---------------------------------------------------------------------------

/// Parse the Flattened Device Tree and populate global platform info.
///
/// Must be called once during early boot, after `init_heap()` (the `fdt`
/// crate itself does not allocate, but `Once` uses a spin-lock internally).
///
/// # Safety
/// `dtb_ptr` must point to a valid FDT blob in memory, or be 0 (in which
/// case QEMU virt defaults are used).
pub fn parse_fdt(dtb_ptr: usize) {
    PLATFORM.call_once(|| build_platform_info(dtb_ptr));
}

fn build_platform_info(dtb_ptr: usize) -> PlatformInfo {
    let mut info = PlatformInfo {
        uart_base: DEFAULT_UART_BASE,
        uart_irq: DEFAULT_UART_IRQ,
        plic_base: DEFAULT_PLIC_BASE,
        plic_size: DEFAULT_PLIC_SIZE,
        virtio_mmio: [None; 8],
        virtio_count: 0,
        memory: [MemRegion { base: 0, size: 0 }; 4],
        memory_count: 0,
        hart_to_cpu: [None; MAX_HARTS],
        cpu_to_hart: [0; MAX_CPUS],
        num_cpus: 0,
        hartids: [0; MAX_CPUS],
    };

    if dtb_ptr == 0 {
        klog!(boot, warn, "FDT: dtb_ptr is null, using QEMU virt defaults");
        apply_defaults(&mut info);
        return info;
    }

    // SAFETY: dtb_ptr was passed from OpenSBI via a1 and points to a valid
    // FDT blob in reserved memory.
    let fdt = match unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8) } {
        Ok(f) => f,
        Err(_) => {
            klog!(
                boot,
                error,
                "FDT: invalid DTB at {:#x}, using defaults",
                dtb_ptr
            );
            apply_defaults(&mut info);
            return info;
        },
    };

    klog!(
        boot,
        info,
        "FDT: parsing DTB at {:#x}, size={}",
        dtb_ptr,
        fdt.total_size()
    );

    // --- CPUs ---
    parse_cpus_from_fdt(&fdt, &mut info);

    // --- Memory ---
    parse_memory_from_fdt(&fdt, &mut info);

    // --- PLIC ---
    parse_plic_from_fdt(&fdt, &mut info);

    // --- UART ---
    parse_uart_from_fdt(&fdt, &mut info);

    // --- VirtIO MMIO ---
    parse_virtio_from_fdt(&fdt, &mut info);

    info
}

fn apply_defaults(info: &mut PlatformInfo) {
    info.memory[0] = MemRegion {
        base: DEFAULT_MEM_BASE,
        size: DEFAULT_MEM_SIZE,
    };
    info.memory_count = 1;
    info.hart_to_cpu[0] = Some(0);
    info.cpu_to_hart[0] = 0;
    info.hartids[0] = 0;
    info.num_cpus = 1;
}

// ---------------------------------------------------------------------------
// Per-subsystem FDT parsing
// ---------------------------------------------------------------------------

fn parse_cpus_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let mut num_cpus = 0usize;

    for cpu in fdt.cpus() {
        let hartid = cpu.ids().first();
        if num_cpus < MAX_CPUS && hartid < MAX_HARTS {
            info.hartids[num_cpus] = hartid;
            info.hart_to_cpu[hartid] = Some(num_cpus);
            info.cpu_to_hart[num_cpus] = hartid;
            num_cpus += 1;
        }
    }

    if num_cpus == 0 {
        klog!(boot, warn, "FDT: no CPUs found, assuming 1 CPU (hart 0)");
        info.hart_to_cpu[0] = Some(0);
        info.cpu_to_hart[0] = 0;
        info.hartids[0] = 0;
        num_cpus = 1;
    }

    info.num_cpus = num_cpus;
    klog!(boot, info, "FDT: discovered {} CPU(s)", num_cpus);
}

fn parse_memory_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let mut count = 0usize;

    let mem = fdt.memory();
    for region in mem.regions() {
        if count >= 4 {
            break;
        }
        let base = region.starting_address as usize;
        let size = region.size.unwrap_or(0);
        if size > 0 {
            info.memory[count] = MemRegion { base, size };
            count += 1;
            klog!(
                boot,
                info,
                "FDT: memory region {}: base={:#x} size={:#x} ({}MB)",
                count - 1,
                base,
                size,
                size / (1024 * 1024)
            );
        }
    }

    if count == 0 {
        klog!(
            boot,
            warn,
            "FDT: no /memory node, using default 128MB @ 0x8000_0000"
        );
        info.memory[0] = MemRegion {
            base: DEFAULT_MEM_BASE,
            size: DEFAULT_MEM_SIZE,
        };
        count = 1;
    }

    info.memory_count = count;
}

fn parse_plic_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let plic_node = fdt.find_compatible(&["sifive,plic-1.0.0", "riscv,plic0"]);

    if let Some(node) = plic_node {
        if let Some(mut regs) = node.reg() {
            if let Some(reg) = regs.next() {
                info.plic_base = reg.starting_address as usize;
                info.plic_size = reg.size.unwrap_or(DEFAULT_PLIC_SIZE);
                klog!(
                    boot,
                    info,
                    "FDT: PLIC at {:#x} size={:#x}",
                    info.plic_base,
                    info.plic_size
                );
                return;
            }
        }
    }

    klog!(
        boot,
        warn,
        "FDT: no PLIC node, using default {:#x}",
        DEFAULT_PLIC_BASE
    );
}

fn parse_uart_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let uart_node = fdt.find_compatible(&["ns16550a"]);

    if let Some(node) = uart_node {
        if let Some(mut regs) = node.reg() {
            if let Some(reg) = regs.next() {
                info.uart_base = reg.starting_address as usize;
            }
        }
        // Read interrupts property (raw big-endian u32)
        if let Some(irq_prop) = node.property("interrupts") {
            if irq_prop.value.len() >= 4 {
                info.uart_irq =
                    u32::from_be_bytes(irq_prop.value[..4].try_into().unwrap_or([0; 4]));
            }
        }
        klog!(
            boot,
            info,
            "FDT: UART at {:#x} irq={}",
            info.uart_base,
            info.uart_irq
        );
        return;
    }

    klog!(
        boot,
        warn,
        "FDT: no UART node, using default {:#x}",
        DEFAULT_UART_BASE
    );
}

fn parse_virtio_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let mut count = 0usize;

    // Try compatible string first, then path-based search
    let nodes = fdt.find_all_nodes("/soc/virtio_mmio");
    for node in nodes {
        if count >= 8 {
            break;
        }
        let mut base = 0usize;
        let mut size = 0usize;
        let mut irq = 0u32;

        if let Some(mut regs) = node.reg() {
            if let Some(reg) = regs.next() {
                base = reg.starting_address as usize;
                size = reg.size.unwrap_or(0x1000);
            }
        }
        if let Some(irq_prop) = node.property("interrupts") {
            if irq_prop.value.len() >= 4 {
                irq = u32::from_be_bytes(irq_prop.value[..4].try_into().unwrap_or([0; 4]));
            }
        }

        if base != 0 {
            info.virtio_mmio[count] = Some(VirtioMmioDevice { base, size, irq });
            count += 1;
        }
    }

    // Fallback: try compatible string search if path yielded nothing
    if count == 0 {
        let compat_nodes = fdt.find_all_nodes("/soc");
        for parent in compat_nodes {
            for child in parent.children() {
                if count >= 8 {
                    break;
                }
                if let Some(compat) = child.compatible() {
                    if compat.all().any(|c| c == "virtio,mmio") {
                        let mut base = 0usize;
                        let mut size = 0usize;
                        let mut irq = 0u32;

                        if let Some(mut regs) = child.reg() {
                            if let Some(reg) = regs.next() {
                                base = reg.starting_address as usize;
                                size = reg.size.unwrap_or(0x1000);
                            }
                        }
                        if let Some(irq_prop) = child.property("interrupts") {
                            if irq_prop.value.len() >= 4 {
                                irq = u32::from_be_bytes(
                                    irq_prop.value[..4].try_into().unwrap_or([0; 4]),
                                );
                            }
                        }

                        if base != 0 {
                            info.virtio_mmio[count] = Some(VirtioMmioDevice { base, size, irq });
                            count += 1;
                        }
                    }
                }
            }
        }
    }

    info.virtio_count = count;
    klog!(
        boot,
        info,
        "FDT: discovered {} VirtIO MMIO device(s)",
        count
    );
    for (i, dev) in info.virtio_mmio.iter().enumerate() {
        if let Some(d) = dev {
            klog!(
                boot,
                info,
                "  virtio {}: base={:#x} size={:#x} irq={}",
                i,
                d.base,
                d.size,
                d.irq
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Public API (backward-compatible)
// ---------------------------------------------------------------------------

/// Look up logical cpu_id from physical hartid.
pub fn hart_to_cpu(hartid: usize) -> Option<usize> {
    if hartid >= MAX_HARTS {
        return None;
    }
    platform().hart_to_cpu[hartid]
}

/// Look up physical hartid from logical cpu_id.
pub fn cpu_to_hart(cpu_id: usize) -> usize {
    assert!(cpu_id < MAX_CPUS, "cpu_id out of range");
    platform().cpu_to_hart[cpu_id]
}
