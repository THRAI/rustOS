use crate::{
    executor::per_cpu::MAX_CPUS,
    hal_common::{Once, PAGE_SIZE},
};

pub const MAX_HARTS: usize = 64;
const MAX_VIRTIO_MMIO: usize = 8;
const MAX_MEMORY_REGIONS: usize = 4;
const LA64_MMIO_DMW_BASE: usize = 0x8000_0000_0000_0000;
const LA64_DIRECT_MAP_LOW_MASK: usize = 0x0000_0000_FFFF_FFFF;
const DEFAULT_BOARD_NAME: &str = "qemu-virt";

const DEFAULT_UART_PHYS_BASE: usize = 0x1fe0_01e0;
const DEFAULT_UART_IRQ: u32 = 66;
const DEFAULT_IRQCHIP_PHYS_BASE: usize = 0x1000_0000;
const DEFAULT_IRQCHIP_SIZE: usize = 0x400;
const DEFAULT_PCI_ECAM_PHYS_BASE: usize = 0x2000_0000;
const DEFAULT_PCI_ECAM_SIZE: usize = 0x0800_0000;
const DEFAULT_PCI_BUS_START: u8 = 0;
const DEFAULT_PCI_BUS_END: u8 = 0x7f;
const DEFAULT_PCI_MMIO_PHYS_BASE: usize = 0x4000_0000;
const DEFAULT_PCI_MMIO_SIZE: usize = 0x4000_0000;
const DEFAULT_MEM0_BASE: usize = 0x0;
const DEFAULT_MEM0_SIZE: usize = 0x1000_0000;
const DEFAULT_MEM1_BASE: usize = 0x8000_0000;
const DEFAULT_MEM1_SIZE: usize = 0x3000_0000;

#[derive(Debug, Copy, Clone)]
pub struct MemRegion {
    pub base: usize,
    pub size: usize,
}

#[derive(Debug, Copy, Clone)]
pub struct VirtioMmioDevice {
    pub base: usize,
    pub size: usize,
    pub irq: u32,
}

pub struct PlatformInfo {
    pub board_name: &'static str,
    pub discovered_from_dtb: bool,
    pub uart_base: usize,
    pub uart_irq: u32,
    pub plic_base: usize,
    pub plic_size: usize,
    pub pci_ecam_base: usize,
    pub pci_ecam_size: usize,
    pub pci_bus_start: u8,
    pub pci_bus_end: u8,
    pub pci_mmio_base: usize,
    pub pci_mmio_size: usize,
    pub has_pci_host: bool,
    pub virtio_mmio: [Option<VirtioMmioDevice>; MAX_VIRTIO_MMIO],
    pub virtio_count: usize,
    pub memory: [MemRegion; MAX_MEMORY_REGIONS],
    pub memory_count: usize,
    pub hart_to_cpu: [Option<usize>; MAX_HARTS],
    pub cpu_to_hart: [usize; MAX_CPUS],
    pub num_cpus: usize,
    pub hartids: [usize; MAX_CPUS],
}

static PLATFORM: Once<PlatformInfo> = Once::new();

#[inline]
const fn mmio_phys_to_virt(pa: usize) -> usize {
    LA64_MMIO_DMW_BASE | pa
}

pub fn parse_boot_platform(boot_info_ptr: usize) {
    PLATFORM.call_once(|| build_platform_info(boot_info_ptr));
}

pub fn platform() -> &'static PlatformInfo {
    PLATFORM
        .get()
        .expect("la64 platform() called before parse_boot_platform()")
}

pub fn boot_id_to_cpu(boot_id: usize) -> Option<usize> {
    if boot_id >= MAX_HARTS {
        return None;
    }
    platform().hart_to_cpu[boot_id]
}

pub fn cpu_to_boot_id(cpu_id: usize) -> usize {
    platform().cpu_to_hart.get(cpu_id).copied().unwrap_or(0)
}

#[inline]
pub const fn canonical_phys_addr(addr: usize) -> usize {
    addr & LA64_DIRECT_MAP_LOW_MASK
}

pub fn frame_allocator_range(kernel_end_raw: usize) -> (usize, usize) {
    let kernel_end = (canonical_phys_addr(kernel_end_raw) + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let pi = platform();
    let mut best_region_end = 0usize;

    for region in pi.memory.iter().take(pi.memory_count) {
        let region_end = region.base.saturating_add(region.size);
        if kernel_end >= region.base && kernel_end < region_end {
            return (kernel_end.max(region.base), region_end);
        }
        best_region_end = best_region_end.max(region_end);
    }

    if best_region_end > kernel_end {
        klog!(
            boot,
            warn,
            "LA64 platform: kernel end {:#x} not inside discovered memory, using best region end {:#x}",
            kernel_end,
            best_region_end
        );
        return (kernel_end, best_region_end);
    }

    let fallback_end = DEFAULT_MEM1_BASE + DEFAULT_MEM1_SIZE;
    klog!(
        boot,
        warn,
        "LA64 platform: no usable memory region for kernel end {:#x}, falling back to default {:#x}..{:#x}",
        kernel_end,
        DEFAULT_MEM0_BASE,
        fallback_end
    );
    (kernel_end.max(DEFAULT_MEM0_BASE), fallback_end)
}

pub fn init_external_irq_this_cpu(_boot_id: usize) {
    // Phase 1 only wires platform discovery. IRQ controller bring-up stays in
    // later phases, so this is intentionally a no-op for now.
}

pub fn boot_secondary_cpus(_num_cpus: usize, _boot_ids: &[usize], _boot_boot_id: usize) {
    // Phase 1 keeps secondary CPUs parked in boot.S. The platform layer now
    // discovers topology correctly, but real SMP bring-up is implemented later.
}

pub fn shutdown() -> ! {
    loop {
        unsafe {
            core::arch::asm!("idle 0");
        }
    }
}

pub fn stop_this_cpu() -> ! {
    loop {
        unsafe {
            core::arch::asm!("idle 0");
        }
    }
}

fn build_platform_info(boot_info_ptr: usize) -> PlatformInfo {
    let mut info = PlatformInfo {
        board_name: DEFAULT_BOARD_NAME,
        discovered_from_dtb: false,
        uart_base: mmio_phys_to_virt(DEFAULT_UART_PHYS_BASE),
        uart_irq: DEFAULT_UART_IRQ,
        plic_base: mmio_phys_to_virt(DEFAULT_IRQCHIP_PHYS_BASE),
        plic_size: DEFAULT_IRQCHIP_SIZE,
        pci_ecam_base: mmio_phys_to_virt(DEFAULT_PCI_ECAM_PHYS_BASE),
        pci_ecam_size: DEFAULT_PCI_ECAM_SIZE,
        pci_bus_start: DEFAULT_PCI_BUS_START,
        pci_bus_end: DEFAULT_PCI_BUS_END,
        pci_mmio_base: DEFAULT_PCI_MMIO_PHYS_BASE,
        pci_mmio_size: DEFAULT_PCI_MMIO_SIZE,
        has_pci_host: true,
        virtio_mmio: [None; MAX_VIRTIO_MMIO],
        virtio_count: 0,
        memory: [MemRegion { base: 0, size: 0 }; MAX_MEMORY_REGIONS],
        memory_count: 0,
        hart_to_cpu: [None; MAX_HARTS],
        cpu_to_hart: [0; MAX_CPUS],
        num_cpus: 0,
        hartids: [0; MAX_CPUS],
    };

    if boot_info_ptr == 0 {
        klog!(boot, warn, "LA64 platform: boot blob is null, using QEMU virt defaults");
        apply_defaults(&mut info);
        log_platform_info(&info);
        return info;
    }

    let Ok(fdt) = (unsafe { fdt::Fdt::from_ptr(boot_info_ptr as *const u8) }) else {
        klog!(
            boot,
            warn,
            "LA64 platform: invalid DTB at {:#x}, using QEMU virt defaults",
            boot_info_ptr
        );
        apply_defaults(&mut info);
        log_platform_info(&info);
        return info;
    };

    klog!(
        boot,
        info,
        "LA64 platform: parsing DTB at {:#x}, size={}",
        boot_info_ptr,
        fdt.total_size()
    );
    info.discovered_from_dtb = true;

    parse_cpus_from_fdt(&fdt, &mut info);
    parse_memory_from_fdt(&fdt, &mut info);
    parse_irqchip_from_fdt(&fdt, &mut info);
    parse_uart_from_fdt(&fdt, &mut info);
    parse_pci_host_from_fdt(&fdt, &mut info);
    parse_virtio_from_fdt(&fdt, &mut info);

    if info.num_cpus == 0 || info.memory_count == 0 {
        klog!(
            boot,
            warn,
            "LA64 platform: incomplete DT discovery, filling remaining fields with defaults"
        );
        fill_missing_defaults(&mut info);
    }

    log_platform_info(&info);
    info
}

fn apply_defaults(info: &mut PlatformInfo) {
    info.pci_ecam_base = mmio_phys_to_virt(DEFAULT_PCI_ECAM_PHYS_BASE);
    info.pci_ecam_size = DEFAULT_PCI_ECAM_SIZE;
    info.pci_bus_start = DEFAULT_PCI_BUS_START;
    info.pci_bus_end = DEFAULT_PCI_BUS_END;
    info.pci_mmio_base = DEFAULT_PCI_MMIO_PHYS_BASE;
    info.pci_mmio_size = DEFAULT_PCI_MMIO_SIZE;
    info.has_pci_host = true;
    info.memory[0] = MemRegion {
        base: DEFAULT_MEM0_BASE,
        size: DEFAULT_MEM0_SIZE,
    };
    info.memory[1] = MemRegion {
        base: DEFAULT_MEM1_BASE,
        size: DEFAULT_MEM1_SIZE,
    };
    info.memory_count = 2;
    info.hart_to_cpu[0] = Some(0);
    info.cpu_to_hart[0] = 0;
    info.hartids[0] = 0;
    info.num_cpus = 1;
}

fn fill_missing_defaults(info: &mut PlatformInfo) {
    if info.memory_count == 0 {
        info.memory[0] = MemRegion {
            base: DEFAULT_MEM0_BASE,
            size: DEFAULT_MEM0_SIZE,
        };
        info.memory[1] = MemRegion {
            base: DEFAULT_MEM1_BASE,
            size: DEFAULT_MEM1_SIZE,
        };
        info.memory_count = 2;
    }

    if info.num_cpus == 0 {
        info.hart_to_cpu[0] = Some(0);
        info.cpu_to_hart[0] = 0;
        info.hartids[0] = 0;
        info.num_cpus = 1;
    }
}

fn parse_cpus_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let mut num_cpus = 0usize;

    for cpu in fdt.cpus() {
        let boot_id = cpu.ids().first();
        if num_cpus >= MAX_CPUS || boot_id >= MAX_HARTS {
            continue;
        }
        info.hartids[num_cpus] = boot_id;
        info.hart_to_cpu[boot_id] = Some(num_cpus);
        info.cpu_to_hart[num_cpus] = boot_id;
        num_cpus += 1;
    }

    if num_cpus == 0 {
        klog!(boot, warn, "LA64 platform: no CPU nodes found");
        return;
    }

    info.num_cpus = num_cpus;
}

fn parse_memory_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let mut count = 0usize;

    for region in fdt.memory().regions() {
        if count >= MAX_MEMORY_REGIONS {
            break;
        }
        let base = region.starting_address as usize;
        let size = region.size.unwrap_or(0);
        if size == 0 {
            continue;
        }
        info.memory[count] = MemRegion { base, size };
        count += 1;
    }

    info.memory_count = count;
}

fn parse_irqchip_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let pch_pic = fdt.find_compatible(&["loongson,pch-pic-1.0"]);
    if let Some(node) = pch_pic {
        if let Some(mut regs) = node.reg() {
            if let Some(reg) = regs.next() {
                let phys = reg.starting_address as usize;
                info.plic_base = mmio_phys_to_virt(phys);
                info.plic_size = reg.size.unwrap_or(DEFAULT_IRQCHIP_SIZE);
                return;
            }
        }
    }

    klog!(
        boot,
        warn,
        "LA64 platform: no pch-pic node, using default irqchip {:#x}",
        DEFAULT_IRQCHIP_PHYS_BASE
    );
}

fn parse_uart_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let uart = fdt.find_compatible(&["ns16550a"]);
    if let Some(node) = uart {
        if let Some(mut regs) = node.reg() {
            if let Some(reg) = regs.next() {
                info.uart_base = mmio_phys_to_virt(reg.starting_address as usize);
            }
        }
        if let Some(irq_prop) = node.property("interrupts") {
            if irq_prop.value.len() >= 4 {
                info.uart_irq = u32::from_be_bytes(irq_prop.value[..4].try_into().unwrap_or([0; 4]));
            }
        }
        return;
    }

    klog!(
        boot,
        warn,
        "LA64 platform: no UART node, using default UART {:#x}",
        DEFAULT_UART_PHYS_BASE
    );
}

fn parse_pci_host_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let Some(node) = fdt.find_compatible(&["pci-host-ecam-generic"]) else {
        return;
    };

    if let Some(mut regs) = node.reg() {
        if let Some(reg) = regs.next() {
            info.pci_ecam_base = mmio_phys_to_virt(reg.starting_address as usize);
            info.pci_ecam_size = reg.size.unwrap_or(0);
            info.has_pci_host = true;
        }
    }

    if let Some(prop) = node.property("bus-range") {
        if prop.value.len() >= 8 {
            info.pci_bus_start = u32::from_be_bytes(prop.value[0..4].try_into().unwrap_or([0; 4])) as u8;
            info.pci_bus_end = u32::from_be_bytes(prop.value[4..8].try_into().unwrap_or([0; 4])) as u8;
        }
    }

    parse_pci_ranges(node.property("ranges").map(|p| p.value), info);
}

fn parse_pci_ranges(ranges: Option<&[u8]>, info: &mut PlatformInfo) {
    let Some(bytes) = ranges else {
        return;
    };
    // QEMU virt uses PCI child addr-cells=3, parent addr-cells=2, size-cells=2.
    // Each range entry is therefore 7 big-endian u32 cells:
    //   child_hi child_mid child_lo parent_hi parent_lo size_hi size_lo
    if bytes.len() % (7 * 4) != 0 {
        return;
    }

    for entry in bytes.chunks_exact(7 * 4) {
        let child_hi = u32::from_be_bytes(entry[0..4].try_into().unwrap_or([0; 4]));
        let _child_mid = u32::from_be_bytes(entry[4..8].try_into().unwrap_or([0; 4]));
        let _child_lo = u32::from_be_bytes(entry[8..12].try_into().unwrap_or([0; 4]));
        let parent_hi = u32::from_be_bytes(entry[12..16].try_into().unwrap_or([0; 4]));
        let parent_lo = u32::from_be_bytes(entry[16..20].try_into().unwrap_or([0; 4]));
        let size_hi = u32::from_be_bytes(entry[20..24].try_into().unwrap_or([0; 4]));
        let size_lo = u32::from_be_bytes(entry[24..28].try_into().unwrap_or([0; 4]));

        // child_hi bit pattern follows the standard PCI range encoding:
        // 0x0100_0000 => I/O, 0x0200_0000 => non-prefetchable MMIO,
        // 0x0300_0000 => prefetchable MMIO.
        let space_code = child_hi & 0x0300_0000;
        if space_code != 0x0200_0000 && space_code != 0x0300_0000 {
            continue;
        }

        let parent_addr = ((parent_hi as u64) << 32) | parent_lo as u64;
        let size = ((size_hi as u64) << 32) | size_lo as u64;
        if parent_addr == 0 || size == 0 {
            continue;
        }

        info.pci_mmio_base = parent_addr as usize;
        info.pci_mmio_size = size as usize;
        return;
    }
}

fn parse_virtio_from_fdt(fdt: &fdt::Fdt, info: &mut PlatformInfo) {
    let mut count = 0usize;

    for node in fdt.all_nodes() {
        if count >= MAX_VIRTIO_MMIO {
            break;
        }
        let Some(compat) = node.compatible() else {
            continue;
        };
        if !compat.all().any(|c| c == "virtio,mmio") {
            continue;
        }

        let mut base = 0usize;
        let mut size = 0usize;
        let mut irq = 0u32;

        if let Some(mut regs) = node.reg() {
            if let Some(reg) = regs.next() {
                base = mmio_phys_to_virt(reg.starting_address as usize);
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

    info.virtio_count = count;
}

fn log_platform_info(info: &PlatformInfo) {
    klog!(
        boot,
        info,
        "LA64 platform: board={} source={} cpus={} uart={:#x} uart_irq={} irqchip={:#x} irqchip_size={:#x} virtio_count={}",
        info.board_name,
        if info.discovered_from_dtb {
            "dtb"
        } else {
            "fallback"
        },
        info.num_cpus,
        info.uart_base,
        info.uart_irq,
        info.plic_base,
        info.plic_size,
        info.virtio_count
    );

    if info.has_pci_host {
        klog!(
            boot,
            info,
            "LA64 platform: pci-ecam base={:#x} size={:#x} bus-range={}..{} pci-mmio={:#x}+{:#x}",
            info.pci_ecam_base,
            info.pci_ecam_size,
            info.pci_bus_start,
            info.pci_bus_end,
            info.pci_mmio_base,
            info.pci_mmio_size
        );
    }

    for (idx, region) in info.memory.iter().take(info.memory_count).enumerate() {
        klog!(
            boot,
            info,
            "LA64 platform: memory[{}] base={:#x} size={:#x}",
            idx,
            region.base,
            region.size
        );
    }

    for cpu_id in 0..info.num_cpus {
        klog!(
            boot,
            info,
            "LA64 platform: cpu{} -> boot_id {}",
            cpu_id,
            info.hartids[cpu_id]
        );
    }
}
