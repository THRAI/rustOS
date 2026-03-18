use crate::{
    hal::trap::{TrapCause, TrapInfo},
    hal_common::{trap_frame::TrapFrame, PAGE_SIZE},
};

const CSR_ECFG: usize = 0x4;
const CSR_EENTRY: usize = 0xc;

const ECFGF_TIMER: usize = 1 << 11;
const ECFGF_IPI: usize = 1 << 12;

const ESTAT_IS_TIMER: usize = 1 << 11;
const ESTAT_IS_IPI: usize = 1 << 12;
const ESTAT_EXC_SHIFT: usize = 16;
const ESTAT_EXC_MASK: usize = 0x3f << ESTAT_EXC_SHIFT;

const EXC_SYSCALL: usize = 0x0b;
const EXC_ADEF: usize = 0x08;
const EXC_ADEM: usize = 0x09;

extern "C" {
    fn __kernel_trap();
    fn __user_trap();
}

pub fn init() {
    set_kernel_trap_entry();
    unsafe {
        core::arch::asm!("csrset {}, {}", in(reg) (ECFGF_TIMER | ECFGF_IPI), const CSR_ECFG);
    }
}

pub fn set_kernel_trap_entry() {
    unsafe {
        core::arch::asm!("csrwr {}, {}", in(reg) (__kernel_trap as *const () as usize), const CSR_EENTRY);
    }
}

pub fn set_user_trap_entry() {
    unsafe {
        core::arch::asm!("csrwr {}, {}", in(reg) (__user_trap as *const () as usize), const CSR_EENTRY);
    }
}

pub fn describe(tf: &TrapFrame) -> TrapInfo {
    let estat = tf.cause_bits();
    let cause = if estat & ESTAT_IS_TIMER != 0 {
        TrapCause::Timer
    } else if estat & ESTAT_IS_IPI != 0 {
        TrapCause::Software
    } else {
        match (estat & ESTAT_EXC_MASK) >> ESTAT_EXC_SHIFT {
            EXC_SYSCALL => TrapCause::Syscall,
            EXC_ADEF => TrapCause::AccessFaultExecute,
            EXC_ADEM => TrapCause::AccessFaultRead,
            EXC_BREAKPOINT => TrapCause::Unknown(EXC_BREAKPOINT),
            code => TrapCause::Unknown(code),
        }
    };

    TrapInfo {
        cause,
        pc: tf.pc(),
        fault_addr: tf.fault_addr() & !(PAGE_SIZE - 1),
    }
}

#[no_mangle]
pub extern "C" fn kernel_trap_handler(frame: &mut TrapFrame) {
    match describe(frame).cause {
        TrapCause::Timer => super::time::handle_timer_irq(),
        TrapCause::Software => super::ipi::handle_ipi(),
        cause => {
            panic!(
                "la64 unhandled kernel trap: cause={:?} pc={:#x} fault_addr={:#x}",
                cause,
                frame.pc(),
                frame.fault_addr(),
            );
        },
    }
}
