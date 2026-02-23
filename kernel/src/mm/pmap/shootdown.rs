//! TLB shootdown via IPI, targeted by pm_active.
//!
//! Each CPU has a per-CPU `ShootdownRequest` slot. The initiator writes
//! the VA range + ASID, sets `pending`, sends an IPI, then spins on `ack`.
//! The remote CPU's IPI handler calls `handle_shootdown_ipi()` which
//! performs the sfence.vma and sets `ack`.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(target_arch = "riscv64")]
use crate::executor::per_cpu::MAX_CPUS;
#[cfg(not(target_arch = "riscv64"))]
const MAX_CPUS: usize = 8;

/// Adaptive flush threshold: if the range covers more than this many pages,
/// flush the entire ASID instead of per-page sfence.vma.
const SHOOTDOWN_PAGE_THRESHOLD: usize = 16;

/// Per-CPU shootdown request slot.
pub struct ShootdownRequest {
    pub pending: AtomicBool,
    pub ack: AtomicBool,
    pub va_start: AtomicUsize,
    pub va_end: AtomicUsize,
    pub asid: AtomicUsize,
    /// If true, flush entire TLB (used for ASID generation rollover).
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

/// Global per-CPU shootdown request array.
static SHOOTDOWN: [ShootdownRequest; MAX_CPUS] = {
    const INIT: ShootdownRequest = ShootdownRequest::new();
    [INIT; MAX_CPUS]
};

/// Check if the current CPU has a pending shootdown request.
#[inline]
pub fn has_pending(cpu_id: usize) -> bool {
    SHOOTDOWN[cpu_id].pending.load(Ordering::Acquire)
}

/// Handle a shootdown IPI on the current CPU.
///
/// Called from the IPI handler. Performs the appropriate sfence.vma
/// and acknowledges the request.
pub fn handle_shootdown_ipi(cpu_id: usize) {
    let req = &SHOOTDOWN[cpu_id];
    if !req.pending.load(Ordering::Acquire) {
        return;
    }

    #[cfg(target_arch = "riscv64")]
    {
        if req.flush_all.load(Ordering::Relaxed) {
            crate::hal::rv64::tlb::flush_all();
        } else {
            let va_start = req.va_start.load(Ordering::Relaxed);
            let va_end = req.va_end.load(Ordering::Relaxed);
            let asid = req.asid.load(Ordering::Relaxed);
            adaptive_flush(va_start, va_end, asid);
        }
    }

    // Acknowledge: clear pending, set ack.
    req.pending.store(false, Ordering::Release);
    req.ack.store(true, Ordering::Release);
}

/// Invalidate TLB entries on remote CPUs where `active[cpu]` is true.
///
/// The caller must also flush the local TLB. This function only handles
/// remote CPUs via IPI.
#[cfg(target_arch = "riscv64")]
pub fn pmap_shootdown(active: &[AtomicBool; MAX_CPUS], va_start: usize, va_end: usize, asid: u16) {
    let local_cpu = crate::executor::per_cpu::current().cpu_id;

    // Flush local TLB first.
    adaptive_flush(va_start, va_end, asid as usize);

    // Send shootdown to each remote CPU that has this pmap active.
    for cpu in 0..MAX_CPUS {
        if cpu == local_cpu {
            continue;
        }
        if !active[cpu].load(Ordering::Acquire) {
            continue;
        }

        let req = &SHOOTDOWN[cpu];
        // Wait for any prior shootdown to complete.
        while req.pending.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        req.va_start.store(va_start, Ordering::Relaxed);
        req.va_end.store(va_end, Ordering::Relaxed);
        req.asid.store(asid as usize, Ordering::Relaxed);
        req.flush_all.store(false, Ordering::Relaxed);
        req.ack.store(false, Ordering::Release);
        req.pending.store(true, Ordering::Release);

        crate::hal::rv64::ipi::send_ipi(cpu);

        // Spin until acknowledged.
        while !req.ack.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
}

/// Broadcast full TLB flush to all CPUs (for ASID generation rollover).
#[cfg(target_arch = "riscv64")]
pub fn ipi_broadcast_flush_all() {
    let local_cpu = crate::executor::per_cpu::current().cpu_id;

    for cpu in 0..MAX_CPUS {
        if cpu == local_cpu {
            continue;
        }

        let req = &SHOOTDOWN[cpu];
        while req.pending.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        req.flush_all.store(true, Ordering::Relaxed);
        req.ack.store(false, Ordering::Release);
        req.pending.store(true, Ordering::Release);

        crate::hal::rv64::ipi::send_ipi(cpu);

        while !req.ack.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
}

/// Adaptive TLB flush: per-page if small range, full ASID flush otherwise.
#[cfg(target_arch = "riscv64")]
fn adaptive_flush(va_start: usize, va_end: usize, asid: usize) {
    let npages = (va_end.saturating_sub(va_start)) / hal_common::PAGE_SIZE;
    if npages <= SHOOTDOWN_PAGE_THRESHOLD {
        let mut va = va_start;
        while va < va_end {
            // sfence.vma addr, asid
            unsafe {
                core::arch::asm!("sfence.vma {}, {}", in(reg) va, in(reg) asid);
            }
            va += hal_common::PAGE_SIZE;
        }
    } else {
        crate::hal::rv64::tlb::flush_asid(asid);
    }
}
