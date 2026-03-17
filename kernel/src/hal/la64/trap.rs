use crate::{
    hal::trap::{TrapCause, TrapInfo},
    hal_common::{trap_frame::TrapFrame, PAGE_SIZE},
};

pub fn init() {}

pub fn set_kernel_trap_entry() {}

pub fn set_user_trap_entry() {}

pub fn describe(tf: &TrapFrame) -> TrapInfo {
    TrapInfo {
        cause: TrapCause::Unknown(tf.scause()),
        pc: tf.pc(),
        fault_addr: tf.stval() & !(PAGE_SIZE - 1),
    }
}
