//! TLB shootdown via IPI, targeted by pm_active.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::executor::MAX_CPUS;

const SHOOTDOWN_PAGE_THRESHOLD: usize = 16;

pub struct ShootdownRequest {
    pub pending: AtomicBool,
    pub ack: AtomicBool,
    pub va_start: AtomicUsize,
    pub va_end: AtomicUsize,
    pub asid: AtomicUsize,
    pub flush_all: AtomicBool,
}

impl ShootdownRequest {
    const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            ack: AtomicBool::new(false),
            va_start: AtomicUsize::new(0),
            va_end: AtomicUsize::new(0),
            asid: AtomicUsize::new(0),
            flush_all: AtomicBool::new(false),
        }
    }
}

static SHOOTDOWN: [ShootdownRequest; MAX_CPUS] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: ShootdownRequest = ShootdownRequest::new();
    [INIT; MAX_CPUS]
};

#[inline]
pub fn has_pending(cpu_id: usize) -> bool {
    SHOOTDOWN[cpu_id].pending.load(Ordering::Acquire)
}

pub fn handle_shootdown_ipi(cpu_id: usize) {
    let req = &SHOOTDOWN[cpu_id];
    if !req.pending.load(Ordering::Acquire) {
        return;
    }

    if req.flush_all.load(Ordering::Relaxed) {
        crate::hal::flush_all();
    } else {
        let va_start = req.va_start.load(Ordering::Relaxed);
        let va_end = req.va_end.load(Ordering::Relaxed);
        let asid = req.asid.load(Ordering::Relaxed);
        adaptive_flush(va_start, va_end, asid);
    }

    req.pending.store(false, Ordering::Release);
    req.ack.store(true, Ordering::Release);
}

pub fn pmap_shootdown(active: &[AtomicBool; MAX_CPUS], va_start: usize, va_end: usize, asid: u16) {
    let local_cpu = crate::executor::current().cpu_id;

    adaptive_flush(va_start, va_end, asid as usize);

    for (cpu, req) in SHOOTDOWN.iter().enumerate() {
        if cpu == local_cpu {
            continue;
        }
        if !active[cpu].load(Ordering::Acquire) {
            continue;
        }
        while req.pending.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        req.va_start.store(va_start, Ordering::Relaxed);
        req.va_end.store(va_end, Ordering::Relaxed);
        req.asid.store(asid as usize, Ordering::Relaxed);
        req.flush_all.store(false, Ordering::Relaxed);
        req.ack.store(false, Ordering::Release);
        req.pending.store(true, Ordering::Release);

        crate::hal::send_ipi(cpu);

        while !req.ack.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
}

pub fn ipi_broadcast_flush_all() {
    let local_cpu = crate::executor::current().cpu_id;

    for (cpu, req) in SHOOTDOWN.iter().enumerate() {
        if cpu == local_cpu {
            continue;
        }
        while req.pending.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        req.flush_all.store(true, Ordering::Relaxed);
        req.ack.store(false, Ordering::Release);
        req.pending.store(true, Ordering::Release);

        crate::hal::send_ipi(cpu);

        while !req.ack.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
}

fn adaptive_flush(va_start: usize, va_end: usize, asid: usize) {
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
