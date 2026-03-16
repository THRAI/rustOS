//! Platform HAL facade.

pub use super::rv64::fdt::{MemRegion, PlatformInfo, VirtioMmioDevice};

#[inline]
pub fn parse_boot_platform(dtb_ptr: usize) {
    super::rv64::fdt::parse_fdt(dtb_ptr);
}

#[inline]
pub fn platform() -> &'static PlatformInfo {
    super::rv64::fdt::platform()
}

#[inline]
pub fn boot_id_to_cpu(boot_id: usize) -> Option<usize> {
    super::rv64::fdt::hart_to_cpu(boot_id)
}

#[inline]
pub fn cpu_to_boot_id(cpu_id: usize) -> usize {
    super::rv64::fdt::cpu_to_hart(cpu_id)
}

#[inline]
pub fn init_external_irq_this_cpu(boot_id: usize) {
    super::rv64::plic::init_hart(boot_id);
}

#[inline]
pub fn boot_secondary_cpus(num_cpus: usize, boot_ids: &[usize], boot_boot_id: usize) {
    super::rv64::smp::boot_secondary_harts(num_cpus, boot_ids, boot_boot_id);
}

#[inline]
pub fn shutdown() -> ! {
    super::rv64::sbi::shutdown()
}

#[inline]
pub fn stop_this_cpu() -> ! {
    let _ = super::rv64::sbi::hart_stop();
    loop {
        super::cpu::idle_once();
    }
}
