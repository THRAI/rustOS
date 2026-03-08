//! rv64 trap setup and dispatch.

use crate::hal_common::TrapFrame;
use core::sync::atomic::Ordering;

// Interrupt bit in scause (bit 63 on rv64)
const SCAUSE_INTERRUPT: usize = 1 << 63;

// Interrupt cause codes
const IRQ_S_SOFTWARE: usize = 1;
const IRQ_S_TIMER: usize = 5;
const IRQ_S_EXTERNAL: usize = 9;

// Exception cause codes
const EXC_LOAD_ACCESS_FAULT: usize = 5;
const EXC_STORE_ACCESS_FAULT: usize = 7;
const EXC_ECALL_U: usize = 8;
const EXC_INST_PAGE_FAULT: usize = 12;
const EXC_LOAD_PAGE_FAULT: usize = 13;
const EXC_STORE_PAGE_FAULT: usize = 15;

extern "C" {
    fn __kernel_trap();
    fn __user_trap();
}

/// Initialize trap infrastructure: set stvec, enable timer + software interrupts in sie.
pub fn init() {
    set_kernel_trap_entry();
    // Enable S-mode timer interrupt (STIE = bit 5), software interrupt (SSIE = bit 1),
    // and external interrupt (SEIE = bit 9) in sie.
    unsafe {
        core::arch::asm!("csrs sie, {}", in(reg) (1usize << 5) | (1usize << 1) | (1usize << 9));
    }
    klog!(trap, info, "stvec set, STIE+SSIE enabled");
}

/// Point stvec to __kernel_trap (Direct mode).
pub fn set_kernel_trap_entry() {
    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __kernel_trap as *const () as usize,
        );
    }
}

/// Point stvec to __user_trap (stub for Phase 3).
pub fn set_user_trap_entry() {
    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __user_trap as *const () as usize,
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
                super::ipi::handle_ipi();
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
                panic!(
                    "[trap] unexpected U-ecall in __kernel_trap: sepc={:#x}, a7={}",
                    frame.pc(),
                    frame.arg(7)
                );
            }
            EXC_LOAD_ACCESS_FAULT
            | EXC_STORE_ACCESS_FAULT
            | EXC_INST_PAGE_FAULT
            | EXC_LOAD_PAGE_FAULT
            | EXC_STORE_PAGE_FAULT => {
                // Check pcb_onfault for exception fixup (copy_user path)
                let pc = crate::executor::per_cpu::current();
                let onfault = pc.pcb_onfault.load(Ordering::Relaxed);
                if onfault != 0 {
                    // Redirect to landing pad: set sepc to onfault address
                    frame.set_pc(onfault);
                    // Clear onfault (landing pad also clears, but belt-and-suspenders)
                    pc.pcb_onfault.store(0, Ordering::Relaxed);
                    return;
                }
                // No fixup — real fault, panic
                panic!(
                    "[trap] page fault: cause={}, stval={:#x}, sepc={:#x}, ra={:#x}, sp={:#x}",
                    code,
                    frame.stval(),
                    frame.pc(),
                    frame.ra(),
                    frame.sp(),
                );
            }
            _ => {
                panic!(
                    "[trap] unhandled exception: cause={}, stval={:#x}, sepc={:#x}, ra={:#x}, sp={:#x}",
                    code,
                    frame.stval(),
                    frame.pc(),
                    frame.ra(),
                    frame.sp(),
                );
            }
        }
    }
}

/// Stub: external interrupt handler (expanded in later phases).
fn handle_external_irq() {
    let hart = crate::executor::per_cpu::current().hartid;
    // FreeBSD-style claim loop: handle all pending IRQs per trap entry
    loop {
        let irq = super::plic::claim(hart);
        if irq == 0 {
            break;
        }
        if irq == super::plic::UART_IRQ {
            // Drain entire UART FIFO (handles interrupt coalescing)
            while let Some(ch) = super::uart::getchar() {
                crate::console::console_irq_push(ch);
            }
        }
        super::plic::complete(hart, irq);
    }
}
