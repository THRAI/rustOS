use core::sync::atomic::{AtomicBool, Ordering};

use crate::executor::MAX_CPUS;

const SHOOTDOWN_PAGE_THRESHOLD: usize = 16;
static WARNED_REMOTE_SHOOTDOWN: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn adaptive_flush(va_start: usize, va_end: usize, asid: usize) {
    let npages = (va_end.saturating_sub(va_start)) / crate::hal_common::PAGE_SIZE;
    if npages <= SHOOTDOWN_PAGE_THRESHOLD {
        let mut va = va_start;
        while va < va_end {
            crate::hal::paging::flush_addr_asid(va, asid);
            va += crate::hal_common::PAGE_SIZE;
        }
    } else {
        crate::hal::flush_asid(asid);
    }
}

#[inline]
pub fn has_pending(_cpu_id: usize) -> bool {
    false
}

pub fn handle_shootdown_ipi(_cpu_id: usize) {
    crate::hal::flush_all();
}

pub fn pmap_shootdown(
    active: &[AtomicBool; MAX_CPUS],
    va_start: usize,
    va_end: usize,
    asid: u16,
) {
    let local_cpu = crate::executor::current().cpu_id;

    adaptive_flush(va_start, va_end, asid as usize);

    let remote_active = active
        .iter()
        .enumerate()
        .any(|(cpu, slot)| cpu != local_cpu && slot.load(Ordering::Acquire));
    if remote_active && !WARNED_REMOTE_SHOOTDOWN.swap(true, Ordering::AcqRel) {
        klog!(
            vm,
            warn,
            "LA64 shootdown: remote active CPUs detected, but IPI-driven remote TLB invalidation is not implemented yet"
        );
    }
}

pub fn ipi_broadcast_flush_all() {
    crate::hal::flush_all();
}
