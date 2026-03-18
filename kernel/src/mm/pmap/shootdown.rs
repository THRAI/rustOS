//! TLB shootdown via IPI, targeted by pm_active.
//!
//! Each CPU has a per-CPU `ShootdownRequest` slot. The initiator writes
//! the VA range + ASID, sets `pending`, sends an IPI, then spins on `ack`.
//! The remote CPU's IPI handler calls `handle_shootdown_ipi()` which
//! performs the sfence.vma and sets `ack`.

#[inline]
pub fn has_pending(cpu_id: usize) -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::shootdown::has_pending(cpu_id)
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::shootdown::has_pending(cpu_id)
    }
}

pub fn handle_shootdown_ipi(cpu_id: usize) {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::shootdown::handle_shootdown_ipi(cpu_id)
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::shootdown::handle_shootdown_ipi(cpu_id)
    }
}

pub fn pmap_shootdown(
    active: &[core::sync::atomic::AtomicBool; crate::executor::MAX_CPUS],
    va_start: usize,
    va_end: usize,
    asid: u16,
) {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::shootdown::pmap_shootdown(active, va_start, va_end, asid)
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::shootdown::pmap_shootdown(active, va_start, va_end, asid)
    }
}

pub fn ipi_broadcast_flush_all() {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::shootdown::ipi_broadcast_flush_all()
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::shootdown::ipi_broadcast_flush_all()
    }
}
