//! Inter-Processor Interrupt (IPI) support via SBI sPI extension.
//!
//! send_ipi wakes a remote CPU from wfi. handle_ipi clears the
//! pending bit -- the actual work happens in the executor loop.

use super::{fdt, sbi};

/// Send an IPI to the target logical CPU, waking it from wfi.
pub fn send_ipi(target_cpu: usize) {
    let hartid = fdt::cpu_to_hart(target_cpu);
    // hart_mask is a bitmask: bit (hartid - hart_mask_base)
    // For simplicity, use hart_mask_base = hartid, mask = 1 (just this hart)
    sbi::send_ipi(1, hartid);
}

/// Handle S-mode software interrupt (IPI received).
/// Called from trap dispatch on scause interrupt code 1.
/// Clears sip.SSIP, then checks for pending TLB shootdown requests.
pub fn handle_ipi() {
    // Clear S-mode software interrupt pending bit (SSIP = bit 1)
    unsafe {
        core::arch::asm!("csrc sip, {}", in(reg) 1usize << 1);
    }

    // Check for pending TLB shootdown on this CPU.
    let cpu_id = crate::executor::current().cpu_id;
    if crate::mm::has_pending(cpu_id) {
        crate::hal::handle_shootdown_ipi(cpu_id);
    }
}
