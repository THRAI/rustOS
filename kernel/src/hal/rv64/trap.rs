//! rv64 trap setup and dispatch.

use crate::kprintln;
use hal_common::TrapFrame;

// Interrupt bit in scause (bit 63 on rv64)
const SCAUSE_INTERRUPT: usize = 1 << 63;

// Interrupt cause codes
const IRQ_S_SOFTWARE: usize = 1;
const IRQ_S_TIMER: usize = 5;
const IRQ_S_EXTERNAL: usize = 9;

// Exception cause codes
const EXC_ECALL_U: usize = 8;
const EXC_INST_PAGE_FAULT: usize = 12;
const EXC_LOAD_PAGE_FAULT: usize = 13;
const EXC_STORE_PAGE_FAULT: usize = 15;

extern "C" {
    fn __kernel_trap();
    fn __user_trap();
}

/// Initialize trap infrastructure: set stvec, enable timer interrupt in sie.
pub fn init() {
    set_kernel_trap_entry();
    // Enable S-mode timer interrupt (STIE = bit 5 of sie)
    unsafe {
        core::arch::asm!("csrs sie, {}", in(reg) 1usize << 5);
    }
    kprintln!("[trap] stvec set, STIE enabled");
}

/// Point stvec to __kernel_trap (Direct mode).
pub fn set_kernel_trap_entry() {
    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __kernel_trap as usize,
        );
    }
}

/// Point stvec to __user_trap (stub for Phase 3).
pub fn set_user_trap_entry() {
    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __user_trap as usize,
        );
    }
}

/// Rust trap handler called from __kernel_trap asm.
#[no_mangle]
pub extern "C" fn kernel_trap_handler(frame: &mut TrapFrame) {
    let scause = frame.scause();
    let is_interrupt = scause & SCAUSE_INTERRUPT != 0;
    let code = scause & !SCAUSE_INTERRUPT;

    if is_interrupt {
        match code {
            IRQ_S_TIMER => {
                super::timer::handle_timer_irq();
            }
            IRQ_S_EXTERNAL => {
                handle_external_irq();
            }
            IRQ_S_SOFTWARE => {
                handle_ipi();
            }
            _ => {
                panic!(
                    "[trap] unhandled interrupt: code={}, sepc={:#x}",
                    code,
                    frame.pc()
                );
            }
        }
    } else {
        match code {
            EXC_ECALL_U => {
                // Stub: ecall from U-mode (Phase 3)
                kprintln!("[trap] ecall from U-mode (stub), sepc={:#x}", frame.pc());
                frame.set_pc(frame.pc() + 4);
            }
            EXC_INST_PAGE_FAULT | EXC_LOAD_PAGE_FAULT | EXC_STORE_PAGE_FAULT => {
                // Stub: page faults (Phase 2)
                panic!(
                    "[trap] page fault: cause={}, stval={:#x}, sepc={:#x}",
                    code,
                    frame.stval(),
                    frame.pc()
                );
            }
            _ => {
                panic!(
                    "[trap] unhandled exception: cause={}, stval={:#x}, sepc={:#x}",
                    code,
                    frame.stval(),
                    frame.pc()
                );
            }
        }
    }
}

/// Stub: external interrupt handler (expanded in later phases).
fn handle_external_irq() {
    kprintln!("[trap] external IRQ (stub)");
}

/// Stub: inter-processor interrupt handler.
fn handle_ipi() {
    // Clear S-mode software interrupt pending bit
    unsafe {
        core::arch::asm!("csrc sip, {}", in(reg) 1usize << 1);
    }
    kprintln!("[trap] IPI received (stub)");
}
