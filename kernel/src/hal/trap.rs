//! Trap facade.

#[cfg(target_arch = "loongarch64")]
pub use super::la64::{
    trap::{init, set_kernel_trap_entry, set_user_trap_entry},
    user_trap::trap_return,
};
#[cfg(target_arch = "riscv64")]
pub use super::rv64::{
    trap::{init, set_kernel_trap_entry, set_user_trap_entry},
    user_trap::trap_return,
};
pub use crate::hal_common::trap_frame::TrapFrame;
use crate::hal_common::PAGE_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapCause {
    Syscall,
    Timer,
    Software,
    External,
    PageFaultRead,
    PageFaultWrite,
    PageFaultExecute,
    AccessFaultRead,
    AccessFaultWrite,
    AccessFaultExecute,
    Unknown(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapInfo {
    pub cause: TrapCause,
    pub pc: usize,
    pub fault_addr: usize,
}

#[inline]
pub fn describe(tf: &TrapFrame) -> TrapInfo {
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::trap::describe(tf);
    }

    #[cfg(target_arch = "riscv64")]
    {
        const SCAUSE_INTERRUPT: usize = 1 << 63;
        const IRQ_S_SOFTWARE: usize = 1;
        const IRQ_S_TIMER: usize = 5;
        const IRQ_S_EXTERNAL: usize = 9;
        const EXC_LOAD_ACCESS_FAULT: usize = 5;
        const EXC_STORE_ACCESS_FAULT: usize = 7;
        const EXC_ECALL_U: usize = 8;
        const EXC_INST_ACCESS_FAULT: usize = 1;
        const EXC_INST_PAGE_FAULT: usize = 12;
        const EXC_LOAD_PAGE_FAULT: usize = 13;
        const EXC_STORE_PAGE_FAULT: usize = 15;

        let scause = tf.scause();
        let code = scause & !SCAUSE_INTERRUPT;
        let cause = if scause & SCAUSE_INTERRUPT != 0 {
            match code {
                IRQ_S_TIMER => TrapCause::Timer,
                IRQ_S_SOFTWARE => TrapCause::Software,
                IRQ_S_EXTERNAL => TrapCause::External,
                _ => TrapCause::Unknown(code),
            }
        } else {
            match code {
                EXC_ECALL_U => TrapCause::Syscall,
                EXC_LOAD_ACCESS_FAULT => TrapCause::AccessFaultRead,
                EXC_STORE_ACCESS_FAULT => TrapCause::AccessFaultWrite,
                EXC_INST_ACCESS_FAULT => TrapCause::AccessFaultExecute,
                EXC_INST_PAGE_FAULT => TrapCause::PageFaultExecute,
                EXC_LOAD_PAGE_FAULT => TrapCause::PageFaultRead,
                EXC_STORE_PAGE_FAULT => TrapCause::PageFaultWrite,
                _ => TrapCause::Unknown(code),
            }
        };

        TrapInfo {
            cause,
            pc: tf.pc(),
            fault_addr: tf.stval() & !(PAGE_SIZE - 1),
        }
    }
}
