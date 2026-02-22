//! rv64 timer driver.
//!
//! Uses SBI set_timer ecall to program stimecmp for next timer IRQ.
//! QEMU virt provides a 10 MHz timebase.

use crate::kprintln;
use core::sync::atomic::{AtomicU64, Ordering};

/// QEMU virt timebase: 10 MHz
pub const TICK_HZ: u64 = 10_000_000;
/// Ticks per millisecond
pub const TICK_MS: u64 = TICK_HZ / 1000;
/// Default timer interval: 10ms per CONTEXT.md
const DEFAULT_INTERVAL: u64 = TICK_MS * 10;

/// Monotonic tick counter (incremented on each timer IRQ).
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Initialize timer: arm the first timer interrupt.
pub fn init() {
    set_oneshot(DEFAULT_INTERVAL);
    kprintln!("[timer] init: {}Hz timebase, {}ms interval", TICK_HZ, DEFAULT_INTERVAL / TICK_MS);
}

/// Read the `time` CSR (cycle counter at timebase frequency).
#[inline]
pub fn read_time() -> u64 {
    let val: u64;
    unsafe {
        core::arch::asm!("csrr {}, time", out(reg) val);
    }
    val
}

/// Program next timer IRQ at `current_time + ticks` via SBI set_timer.
pub fn set_oneshot(ticks: u64) {
    let deadline = read_time() + ticks;
    super::sbi::set_timer(deadline);
}

/// Handle S-mode timer interrupt. Called from trap dispatch.
pub fn handle_timer_irq() {
    let tick = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    // Re-arm timer for next interval
    set_oneshot(DEFAULT_INTERVAL);
    // Periodic status (every 100 ticks = ~1 second)
    if tick % 100 == 0 {
        kprintln!("[timer] tick {}", tick);
    }
}

/// Get current tick count.
#[inline]
pub fn tick_count() -> u64 {
    TICK_COUNT.load(Ordering::Relaxed)
}
