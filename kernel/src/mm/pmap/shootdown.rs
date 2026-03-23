//! TLB shootdown via IPI, targeted by pm_active.
//!
//! Each CPU has a per-CPU `ShootdownRequest` slot. The initiator writes
//! the VA range + ASID, sets `pending`, sends an IPI, then spins on `ack`.
//! The remote CPU's IPI handler calls `handle_shootdown_ipi()` which
//! performs the sfence.vma and sets `ack`.
//!
//! ## Quiesce Protocol (C-4 fix)
//!
//! For batched PTE modifications (`pmap_protect`, `pmap_remove`), a simple
//! post-hoc shootdown leaves a window where remote harts execute user code
//! with stale TLB entries.  The quiesce protocol parks remote harts in
//! kernel mode with interrupts disabled **before** any PTE is modified:
//!
//! 1. Initiator sets `quiesce=true, pending=true` on each active remote CPU.
//! 2. Initiator sends IPIs to all active remote CPUs (parallel dispatch).
//! 3. Initiator waits for all `ack=true` (remote harts are parked).
//! 4. Initiator modifies PTEs + flushes local TLB.
//! 5. Initiator drops `QuiesceGuard` → clears `pending` on each slot.
//! 6. Remote harts see `pending=false`, flush local TLB, restore interrupts,
//!    resume.  Next user-mode access takes a TLB miss and walks fresh PTEs.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(target_arch = "riscv64")]
use crate::executor::MAX_CPUS;
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
    /// If true, this is a quiesce request: ack immediately, then spin
    /// with interrupts disabled until `pending` is cleared by the initiator.
    pub quiesce: AtomicBool,
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
            quiesce: AtomicBool::new(false),
        }
    }
}

/// Global per-CPU shootdown request array.
static SHOOTDOWN: [ShootdownRequest; MAX_CPUS] = {
    #[allow(clippy::declare_interior_mutable_const)]
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
/// and acknowledges the request. For quiesce requests, parks the hart
/// with interrupts disabled until the initiator releases it.
pub fn handle_shootdown_ipi(cpu_id: usize) {
    let req = &SHOOTDOWN[cpu_id];
    if !req.pending.load(Ordering::Acquire) {
        return;
    }

    // Quiesce path: park this hart with interrupts disabled until the
    // initiator finishes modifying PTEs and drops the QuiesceGuard.
    if req.quiesce.load(Ordering::Acquire) {
        // Tell the initiator we're parked.
        req.ack.store(true, Ordering::Release);

        // SAFETY: Disable S-mode interrupts to prevent timer IRQs from
        // triggering context switches while we're quiesced. Save old
        // sstatus so we restore the original SIE state on exit.
        #[cfg(target_arch = "riscv64")]
        let old_sstatus: usize;
        #[cfg(target_arch = "riscv64")]
        unsafe {
            // csrrc: read sstatus, then clear SIE (bit 1). Returns old value.
            core::arch::asm!("csrrc {}, sstatus, {}", out(reg) old_sstatus, in(reg) 1usize << 1);
        }

        // Spin until the initiator releases us by clearing `pending`.
        while req.pending.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        // Flush entire local TLB — the initiator modified PTEs while we
        // were parked, so any cached translations may be stale.
        #[cfg(target_arch = "riscv64")]
        crate::hal::flush_all();

        // Restore original interrupt state.
        #[cfg(target_arch = "riscv64")]
        unsafe {
            // Only re-enable SIE if it was enabled before we disabled it.
            core::arch::asm!("csrs sstatus, {}", in(reg) old_sstatus & (1usize << 1));
        }

        req.quiesce.store(false, Ordering::Release);
        return;
    }

    // Normal shootdown path: flush TLB, then ack.
    #[cfg(target_arch = "riscv64")]
    {
        if req.flush_all.load(Ordering::Relaxed) {
            crate::hal::flush_all();
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
/// Used for single-page operations (`pmap_enter`, `pte_bit_clear`) where
/// the quiesce overhead is not justified.  For batched operations
/// (`pmap_protect`, `pmap_remove`), use `quiesce_remote_harts` instead.
#[cfg(target_arch = "riscv64")]
pub fn pmap_shootdown(active: &[AtomicBool; MAX_CPUS], va_start: usize, va_end: usize, asid: u16) {
    let local_cpu = crate::executor::current().cpu_id;

    // Flush local TLB first.
    adaptive_flush(va_start, va_end, asid as usize);

    // Send shootdown to each remote CPU that has this pmap active.
    for (cpu, req) in SHOOTDOWN.iter().enumerate() {
        if cpu == local_cpu {
            continue;
        }
        if !active[cpu].load(Ordering::Acquire) {
            continue;
        }
        // Wait for any prior shootdown to complete.
        while req.pending.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }

        req.va_start.store(va_start, Ordering::Relaxed);
        req.va_end.store(va_end, Ordering::Relaxed);
        req.asid.store(asid as usize, Ordering::Relaxed);
        req.flush_all.store(false, Ordering::Relaxed);
        req.quiesce.store(false, Ordering::Relaxed);
        req.ack.store(false, Ordering::Release);
        req.pending.store(true, Ordering::Release);

        crate::hal::send_ipi(cpu);

        // Spin until acknowledged.
        while !req.ack.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
}

#[cfg(target_arch = "loongarch64")]
pub fn pmap_shootdown(active: &[AtomicBool; MAX_CPUS], va_start: usize, va_end: usize, asid: u16) {
    crate::hal::la64::paging::shootdown::pmap_shootdown(active, va_start, va_end, asid);
}

/// Broadcast full TLB flush to all CPUs (for ASID generation rollover).
#[cfg(target_arch = "riscv64")]
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
        req.quiesce.store(false, Ordering::Relaxed);
        req.ack.store(false, Ordering::Release);
        req.pending.store(true, Ordering::Release);

        crate::hal::send_ipi(cpu);

        while !req.ack.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }
}

#[cfg(target_arch = "loongarch64")]
pub fn ipi_broadcast_flush_all() {
    crate::hal::la64::paging::shootdown::ipi_broadcast_flush_all();
}

// ---------------------------------------------------------------------------
// Quiesce protocol for batched PTE modifications (C-4 fix)
// ---------------------------------------------------------------------------

/// RAII guard that releases quiesced remote harts on drop.
///
/// While this guard is alive, all remote harts that had the relevant pmap
/// active are parked in kernel mode with interrupts disabled. The
/// initiator can safely modify PTEs without TLB consistency concerns.
#[cfg(target_arch = "riscv64")]
pub struct QuiesceGuard {
    /// Bitmask of CPU IDs that were quiesced (and need releasing).
    quiesced_mask: u64,
}

#[cfg(target_arch = "riscv64")]
impl QuiesceGuard {
    /// Quiesce all remote harts that have this pmap active.
    ///
    /// Sends quiesce IPIs to all active remote harts in parallel,
    /// then waits for all acks. Each remote hart will:
    /// 1. Trap into kernel (if in user mode) or handle IPI (if in kernel).
    /// 2. Disable interrupts (`csrrc sstatus, SIE`).
    /// 3. Ack (so we know it's parked).
    /// 4. Spin on `pending` until we release it.
    pub fn new(active: &[AtomicBool; MAX_CPUS]) -> Self {
        let local_cpu = crate::executor::current().cpu_id;
        let mut quiesced_mask: u64 = 0;

        // Phase 1: Fire IPIs to all active remote harts (parallel dispatch).
        for (cpu, req) in SHOOTDOWN.iter().enumerate() {
            if cpu == local_cpu {
                continue;
            }
            if !active[cpu].load(Ordering::Acquire) {
                continue;
            }
            // Wait for any prior shootdown to complete.
            while req.pending.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }

            req.quiesce.store(true, Ordering::Relaxed);
            req.flush_all.store(false, Ordering::Relaxed);
            req.ack.store(false, Ordering::Release);
            req.pending.store(true, Ordering::Release);

            crate::hal::send_ipi(cpu);
            quiesced_mask |= 1u64 << cpu;
        }

        // Phase 2: Wait for all acks (all remote harts are parked).
        for (cpu, req) in SHOOTDOWN.iter().enumerate() {
            if quiesced_mask & (1u64 << cpu) == 0 {
                continue;
            }
            while !req.ack.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }

        Self { quiesced_mask }
    }
}

#[cfg(target_arch = "riscv64")]
impl Drop for QuiesceGuard {
    fn drop(&mut self) {
        // Release all quiesced harts by clearing `pending`.
        // Each hart will: flush_all, restore SIE, return from IPI handler.
        for (cpu, req) in SHOOTDOWN.iter().enumerate() {
            if self.quiesced_mask & (1u64 << cpu) == 0 {
                continue;
            }
            req.pending.store(false, Ordering::Release);
        }
    }
}

// ---------------------------------------------------------------------------
// ShootdownBatch — batched TLB invalidation with quiesce protocol
// ---------------------------------------------------------------------------

/// Batched TLB invalidation with quiesce protocol.
/// 1. `new()` → `QuiesceGuard` parks remote harts
/// 2. `add()` → collect `ShootdownToken`s from entry mutations
/// 3. `drop()` → adaptive local flush + release remote harts (they `flush_all`)
#[cfg(target_arch = "riscv64")]
pub struct ShootdownBatch {
    dirty_vas: alloc::vec::Vec<usize>,
    asid: u16,
    _quiesce: QuiesceGuard,
}

#[cfg(target_arch = "riscv64")]
impl ShootdownBatch {
    pub fn new(active: &[AtomicBool; MAX_CPUS], asid: u16) -> Self {
        Self {
            dirty_vas: alloc::vec::Vec::new(),
            asid,
            _quiesce: QuiesceGuard::new(active),
        }
    }

    pub fn add(&mut self, token: super::container::ShootdownToken) {
        self.dirty_vas.push(token.va().as_usize());
    }
}

#[cfg(target_arch = "riscv64")]
impl Drop for ShootdownBatch {
    fn drop(&mut self) {
        if !self.dirty_vas.is_empty() {
            if self.dirty_vas.len() <= SHOOTDOWN_PAGE_THRESHOLD {
                for &va in &self.dirty_vas {
                    // SAFETY: sfence.vma with specific VA and ASID is always safe
                    // when executed on the local hart. We hold the quiesce guard
                    // so remote harts are parked.
                    unsafe {
                        core::arch::asm!("sfence.vma {}, {}", in(reg) va, in(reg) self.asid as usize);
                    }
                }
            } else {
                crate::hal::flush_asid(self.asid as usize);
            }
        }
        // _quiesce drops here → releases remote harts → they flush_all
    }
}

#[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
pub struct ShootdownBatch;

#[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
impl ShootdownBatch {
    pub fn new(_active: &[AtomicBool; MAX_CPUS], _asid: u16) -> Self {
        Self
    }
    pub fn add(&mut self, _token: super::container::ShootdownToken) {}
}

#[cfg(target_arch = "loongarch64")]
pub struct ShootdownBatch {
    dirty_vas: alloc::vec::Vec<usize>,
    asid: u16,
}

#[cfg(target_arch = "loongarch64")]
impl ShootdownBatch {
    pub fn new(_active: &[AtomicBool; MAX_CPUS], asid: u16) -> Self {
        Self {
            dirty_vas: alloc::vec::Vec::new(),
            asid,
        }
    }

    pub fn add(&mut self, token: super::container::ShootdownToken) {
        self.dirty_vas.push(token.va().as_usize());
    }
}

#[cfg(target_arch = "loongarch64")]
impl Drop for ShootdownBatch {
    fn drop(&mut self) {
        if self.dirty_vas.is_empty() {
            return;
        }

        if self.dirty_vas.len() <= SHOOTDOWN_PAGE_THRESHOLD {
            for &va in &self.dirty_vas {
                crate::hal::paging::flush_addr_asid(va, self.asid as usize);
            }
        } else if let (Some(&min_va), Some(&max_va)) =
            (self.dirty_vas.iter().min(), self.dirty_vas.iter().max())
        {
            crate::hal::la64::paging::shootdown::adaptive_flush(
                min_va,
                max_va + crate::hal_common::PAGE_SIZE,
                self.asid as usize,
            );
        }
    }
}

/// Adaptive TLB flush: per-page if small range, full ASID flush otherwise.
///
/// Used for local TLB flush after PTE modifications. Also called by the
/// IPI handler on remote harts for normal (non-quiesce) shootdowns.
#[cfg(target_arch = "riscv64")]
pub(super) fn adaptive_flush(va_start: usize, va_end: usize, asid: usize) {
    let npages = (va_end.saturating_sub(va_start)) / crate::hal_common::PAGE_SIZE;
    if npages <= SHOOTDOWN_PAGE_THRESHOLD {
        let mut va = va_start;
        while va < va_end {
            // sfence.vma addr, asid
            unsafe {
                core::arch::asm!("sfence.vma {}, {}", in(reg) va, in(reg) asid);
            }
            va += crate::hal_common::PAGE_SIZE;
        }
    } else {
        crate::hal::flush_asid(asid);
    }
}
