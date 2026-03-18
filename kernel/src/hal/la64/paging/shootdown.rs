use core::sync::atomic::AtomicBool;

#[inline]
pub fn has_pending(_cpu_id: usize) -> bool {
    false
}

pub fn handle_shootdown_ipi(_cpu_id: usize) {
    crate::hal::flush_all();
}

pub fn pmap_shootdown(
    _active: &[AtomicBool; crate::executor::MAX_CPUS],
    va_start: usize,
    va_end: usize,
    asid: u16,
) {
    let mut va = va_start;
    while va < va_end {
        crate::hal::paging::flush_addr_asid(va, asid as usize);
        va += crate::hal_common::PAGE_SIZE;
    }
}

pub fn ipi_broadcast_flush_all() {
    crate::hal::flush_all();
}
