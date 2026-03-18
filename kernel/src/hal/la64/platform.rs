use crate::{executor::per_cpu::MAX_CPUS, hal_common::Once};

pub const MAX_HARTS: usize = 64;
const MAX_VIRTIO_MMIO: usize = 8;
const MAX_MEMORY_REGIONS: usize = 4;
const QEMU_VIRT_UART_BASE: usize = 0x1fe0_01e0;
const QEMU_VIRT_UART_IRQ: u32 = 66;

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
    pub uart_base: usize,
    pub uart_irq: u32,
    pub plic_base: usize,
    pub plic_size: usize,
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

pub fn parse_boot_platform(_boot_info_ptr: usize) {
    PLATFORM.call_once(|| PlatformInfo {
        uart_base: QEMU_VIRT_UART_BASE,
        uart_irq: QEMU_VIRT_UART_IRQ,
        plic_base: 0,
        plic_size: 0,
        virtio_mmio: [None; MAX_VIRTIO_MMIO],
        virtio_count: 0,
        memory: [MemRegion { base: 0, size: 0 }; MAX_MEMORY_REGIONS],
        memory_count: 0,
        hart_to_cpu: {
            let mut map = [None; MAX_HARTS];
            map[0] = Some(0);
            map
        },
        cpu_to_hart: [0; MAX_CPUS],
        num_cpus: 1,
        hartids: [0; MAX_CPUS],
    });
}

pub fn platform() -> &'static PlatformInfo {
    PLATFORM
        .get()
        .expect("la64 platform() called before parse_boot_platform()")
}

pub fn boot_id_to_cpu(boot_id: usize) -> Option<usize> {
    platform().hart_to_cpu.get(boot_id).copied().flatten()
}

pub fn cpu_to_boot_id(cpu_id: usize) -> usize {
    platform().cpu_to_hart.get(cpu_id).copied().unwrap_or(0)
}

pub fn init_external_irq_this_cpu(_boot_id: usize) {}

pub fn boot_secondary_cpus(_num_cpus: usize, _boot_ids: &[usize], _boot_boot_id: usize) {}

pub fn shutdown() -> ! {
    panic!("la64 shutdown is not implemented")
}

pub fn stop_this_cpu() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
