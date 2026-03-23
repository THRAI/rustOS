//! Platform HAL facade.

#[cfg(target_arch = "loongarch64")]
pub use super::la64::platform::{MemRegion, PlatformInfo, VirtioMmioDevice};
#[cfg(target_arch = "riscv64")]
pub use super::rv64::fdt::{MemRegion, PlatformInfo, VirtioMmioDevice};

#[inline]
pub fn parse_boot_platform(dtb_ptr: usize) {
    #[cfg(target_arch = "riscv64")]
    super::rv64::fdt::parse_fdt(dtb_ptr);
    #[cfg(target_arch = "loongarch64")]
    super::la64::platform::parse_boot_platform(dtb_ptr);
}

#[inline]
pub fn platform() -> &'static PlatformInfo {
    #[cfg(target_arch = "riscv64")]
    {
        return super::rv64::fdt::platform();
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::platform::platform();
    }
}

#[inline]
pub fn boot_id_to_cpu(boot_id: usize) -> Option<usize> {
    #[cfg(target_arch = "riscv64")]
    {
        return super::rv64::fdt::hart_to_cpu(boot_id);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::platform::boot_id_to_cpu(boot_id);
    }
}

#[inline]
pub fn cpu_to_boot_id(cpu_id: usize) -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        return super::rv64::fdt::cpu_to_hart(cpu_id);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::platform::cpu_to_boot_id(cpu_id);
    }
}

#[inline]
pub fn init_external_irq_this_cpu(boot_id: usize) {
    #[cfg(target_arch = "riscv64")]
    super::rv64::plic::init_hart(boot_id);
    #[cfg(target_arch = "loongarch64")]
    super::la64::platform::init_external_irq_this_cpu(boot_id);
}

#[inline]
pub fn boot_secondary_cpus(num_cpus: usize, boot_ids: &[usize], boot_boot_id: usize) {
    #[cfg(target_arch = "riscv64")]
    super::rv64::smp::boot_secondary_harts(num_cpus, boot_ids, boot_boot_id);
    #[cfg(target_arch = "loongarch64")]
    super::la64::platform::boot_secondary_cpus(num_cpus, boot_ids, boot_boot_id);
}

#[inline]
pub fn shutdown() -> ! {
    #[cfg(target_arch = "riscv64")]
    {
        super::rv64::sbi::shutdown()
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::platform::shutdown()
    }
}

#[inline]
pub fn stop_this_cpu() -> ! {
    #[cfg(target_arch = "riscv64")]
    {
        let _ = super::rv64::sbi::hart_stop();
        loop {
            super::cpu::idle_once();
        }
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::platform::stop_this_cpu()
    }
}
