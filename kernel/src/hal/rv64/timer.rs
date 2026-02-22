//! rv64 timer driver.
//!
//! Full implementation in Task 2. This stub provides handle_timer_irq()
//! so trap dispatch compiles.

use crate::kprintln;

/// QEMU virt timebase: 10 MHz
pub const TICK_HZ: u64 = 10_000_000;
/// Ticks per millisecond
pub const TICK_MS: u64 = TICK_HZ / 1000;

/// Handle S-mode timer interrupt. Called from trap dispatch.
pub fn handle_timer_irq() {
    // Stub: will be expanded in Task 2 with set_oneshot + tick counter
    kprintln!("[timer] tick (stub)");
}
