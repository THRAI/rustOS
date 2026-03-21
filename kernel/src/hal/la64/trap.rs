use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    hal::trap::{TrapCause, TrapInfo},
    hal_common::{trap_frame::TrapFrame, PAGE_SIZE},
};

const CSR_ECFG: usize = 0x4;
const CSR_EENTRY: usize = 0xc;
const EENTRY_ALIGN_MASK: usize = !0xfffusize;
const CSR_WRITE_ALL_MASK: usize = usize::MAX;
const ECFG_VS_SHIFT: usize = 16;
const ECFG_VS_MASK: usize = 0x7 << ECFG_VS_SHIFT;

const ECFGF_TIMER: usize = 1 << 11;
const ECFGF_IPI: usize = 1 << 12;
const ECFGF_SWI0: usize = 1 << 0;
const ECFGF_HWI0: usize = 1 << 2;

const ESTAT_IS_TIMER: usize = 1 << 11;
const ESTAT_IS_IPI: usize = 1 << 12;
const ESTAT_IS_SWI_MASK: usize = 0x3;
const ESTAT_IS_HWI0: usize = 1 << 2;
const ESTAT_IS_HWI_MASK: usize = 0xff << 2;
const ESTAT_EXC_SHIFT: usize = 16;
const ESTAT_EXC_MASK: usize = 0x3f << ESTAT_EXC_SHIFT;

const EXC_SYSCALL: usize = 0x0b;
const EXC_ADEF: usize = 0x08;
const EXC_ADEM: usize = 0x09;
const EXC_FETCH_PAGE_FAULT: usize = 0x03;
const EXC_LOAD_PAGE_FAULT: usize = 0x01;
const EXC_STORE_PAGE_FAULT: usize = 0x02;
const EXC_FETCH_ADDR_ERROR: usize = 0x0d;
const EXC_FETCH_TLB_REFILL: usize = 0x3f;
const EXC_BREAKPOINT: usize = 0x0c;

extern "C" {
    fn __kernel_trap();
    fn __user_trap();
    static __la64_kernel_trap_count: usize;
    static __la64_user_trap_count: usize;
}

static TIMER_CAUSE_COUNT: AtomicU64 = AtomicU64::new(0);
static SOFTWARE_CAUSE_COUNT: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_CAUSE_COUNT: AtomicU64 = AtomicU64::new(0);
static SYSCALL_CAUSE_COUNT: AtomicU64 = AtomicU64::new(0);
static IGNORED_EXEC_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    set_kernel_trap_entry();
    let ecfg: usize;
    let new_ecfg: usize;
    unsafe {
        core::arch::asm!("csrrd {}, {}", out(reg) ecfg, const CSR_ECFG);
        new_ecfg = ecfg | ECFGF_TIMER | ECFGF_IPI | ECFGF_SWI0 | ECFGF_HWI0;
        core::arch::asm!("csrwr {}, {}", in(reg) new_ecfg, const CSR_ECFG);
    }
}

pub fn set_kernel_trap_entry() {
    let mut ecfg: usize;
    let entry = (__kernel_trap as *const () as usize) & EENTRY_ALIGN_MASK;
    let mut write_val = entry;
    unsafe {
        core::arch::asm!("csrrd {}, {}", out(reg) ecfg, const CSR_ECFG);
        ecfg &= !ECFG_VS_MASK;
        core::arch::asm!("csrwr {}, {}", in(reg) ecfg, const CSR_ECFG);
        core::arch::asm!(
            "csrxchg {}, {}, {}",
            inout(reg) write_val,
            in(reg) CSR_WRITE_ALL_MASK,
            const CSR_EENTRY
        );
    }
}

#[inline]
pub fn clear_swi0_probe() {
    let estat: usize;
    let v: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x5", out(reg) estat);
        v = estat & !1usize;
        core::arch::asm!("csrwr {}, 0x5", in(reg) v);
    }
}

pub fn set_user_trap_entry() {
    let mut ecfg: usize;
    let entry = (__user_trap as *const () as usize) & EENTRY_ALIGN_MASK;
    let mut write_val = entry;
    unsafe {
        core::arch::asm!("csrrd {}, {}", out(reg) ecfg, const CSR_ECFG);
        ecfg &= !ECFG_VS_MASK;
        core::arch::asm!("csrwr {}, {}", in(reg) ecfg, const CSR_ECFG);
        core::arch::asm!(
            "csrxchg {}, {}, {}",
            inout(reg) write_val,
            in(reg) CSR_WRITE_ALL_MASK,
            const CSR_EENTRY
        );
    }
}

#[inline]
pub fn trigger_swi0_probe() {
    let estat: usize;
    let v: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x5", out(reg) estat);
        v = estat | 1;
        core::arch::asm!("csrwr {}, 0x5", in(reg) v);
    }
}

#[inline]
pub fn trap_counts() -> (usize, usize) {
    unsafe { (__la64_kernel_trap_count, __la64_user_trap_count) }
}

#[inline]
pub fn trap_cause_counts() -> (u64, u64, u64, u64, u64) {
    (
        TIMER_CAUSE_COUNT.load(Ordering::Relaxed),
        SOFTWARE_CAUSE_COUNT.load(Ordering::Relaxed),
        EXTERNAL_CAUSE_COUNT.load(Ordering::Relaxed),
        SYSCALL_CAUSE_COUNT.load(Ordering::Relaxed),
        IGNORED_EXEC_FAULT_COUNT.load(Ordering::Relaxed),
    )
}

pub fn describe(tf: &TrapFrame) -> TrapInfo {
    let estat = tf.cause_bits();
    let cause = if estat & ESTAT_IS_TIMER != 0 || estat & ESTAT_IS_HWI0 != 0 {
        TrapCause::Timer
    } else if estat & ESTAT_IS_IPI != 0 || estat & ESTAT_IS_SWI_MASK != 0 {
        TrapCause::Software
    } else if estat & ESTAT_IS_HWI_MASK != 0 {
        TrapCause::External
    } else {
        match (estat & ESTAT_EXC_MASK) >> ESTAT_EXC_SHIFT {
            EXC_SYSCALL => TrapCause::Syscall,
            EXC_FETCH_PAGE_FAULT => TrapCause::PageFaultExecute,
            EXC_LOAD_PAGE_FAULT => TrapCause::PageFaultRead,
            EXC_STORE_PAGE_FAULT => TrapCause::PageFaultWrite,
            EXC_FETCH_TLB_REFILL => TrapCause::PageFaultExecute,
            EXC_FETCH_ADDR_ERROR => TrapCause::AccessFaultExecute,
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
        TrapCause::Syscall => {
            SYSCALL_CAUSE_COUNT.fetch_add(1, Ordering::Relaxed);
            frame.advance_syscall_pc()
        },
        TrapCause::Timer => {
            TIMER_CAUSE_COUNT.fetch_add(1, Ordering::Relaxed);
            super::time::handle_timer_irq()
        },
        TrapCause::Software => {
            SOFTWARE_CAUSE_COUNT.fetch_add(1, Ordering::Relaxed);
            clear_swi0_probe();
            super::ipi::handle_ipi()
        },
        TrapCause::External => {
            EXTERNAL_CAUSE_COUNT.fetch_add(1, Ordering::Relaxed);
        },
        TrapCause::AccessFaultExecute if frame.pc() == frame.fault_addr() => {
            IGNORED_EXEC_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
        },
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
